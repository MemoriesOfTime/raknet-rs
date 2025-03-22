use std::collections::{BinaryHeap, VecDeque};
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::time::{Duration, Instant};
use std::{cmp, io};

use futures::Sink;
use log::trace;
use pin_project_lite::pin_project;

use crate::congestion::legacy::LegacyCongester;
use crate::congestion::CongestionController;
use crate::estimator::{Estimator, RFC6298Impl};
use crate::link::SharedLink;
use crate::opts::FlushStrategy;
use crate::packet::connected::{self, AckOrNack, Frame, FrameSet, Frames, FramesRef, Record};
use crate::packet::{Packet, FRAME_SET_HEADER_SIZE};
use crate::utils::{combine_hashes, u24, Reactor};
use crate::{HashMap, Peer, Priority, Role};

// A frame with penalty
#[derive(Debug)]
struct PenaltyFrame {
    penalty: u8,
    frame: Frame,
}

impl PartialEq for PenaltyFrame {
    fn eq(&self, other: &Self) -> bool {
        self.penalty == other.penalty
    }
}

impl Eq for PenaltyFrame {}

impl PartialOrd for PenaltyFrame {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PenaltyFrame {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        // reverse ordering by penalty
        other.penalty.cmp(&self.penalty)
    }
}

#[derive(Debug)]
pub(crate) struct SendBuffer {
    // ordered by tier
    send_high: BinaryHeap<PenaltyFrame>,
    send_medium: VecDeque<Frame>,
    send_low: BinaryHeap<PenaltyFrame>,
    resend: BinaryHeap<PenaltyFrame>,
}

impl SendBuffer {
    fn new() -> Self {
        Self {
            send_high: BinaryHeap::new(),
            send_medium: VecDeque::new(),
            send_low: BinaryHeap::new(),
            resend: BinaryHeap::new(),
        }
    }

    fn send(&mut self, (priority, frame): (Priority, Frame)) {
        match priority {
            Priority::High(penalty) => self.send_high.push(PenaltyFrame { penalty, frame }),
            Priority::Medium => self.send_medium.push_back(frame),
            Priority::Low(penalty) => self.send_low.push(PenaltyFrame { penalty, frame }),
        }
    }

    fn resend(&mut self, frames: impl IntoIterator<Item = PenaltyFrame>) {
        // the maximum penalty for resending frames
        // the default maximum penalty 255 (limited by u8) is used for priority frames
        const MAX_RESEND_PENALTY: u8 = 30;

        self.resend.extend(frames.into_iter().map(|mut frame| {
            // add penalty while resending
            frame.penalty = cmp::min(frame.penalty.saturating_add(1), MAX_RESEND_PENALTY);
            frame
        }));
    }

    fn pop(&mut self, mtu: usize, reliable: &mut bool, penalty: &mut u8, frames: &mut Frames) {
        let mut remain = mtu - FRAME_SET_HEADER_SIZE;
        let mut penalty_sum: usize = 0;

        // pop resend first
        while let Some(item) = self.resend.peek() {
            if remain >= item.frame.size() {
                if item.frame.flags.reliability.is_reliable() {
                    *reliable = true;
                }
                remain -= item.frame.size();
                let frame = self.resend.pop().unwrap();
                frames.push(frame.frame);
                // inherit the resend penalty
                penalty_sum += frame.penalty as usize;
                continue;
            }
            break;
        }

        // pop send in order

        while let Some(item) = self.send_high.peek() {
            if remain >= item.frame.size() {
                if item.frame.flags.reliability.is_reliable() {
                    *reliable = true;
                }
                remain -= item.frame.size();
                frames.push(self.send_high.pop().unwrap().frame);
                // reset penalty
                penalty_sum = 0;
                continue;
            }
            break;
        }

        while let Some(frame) = self.send_medium.front() {
            if remain >= frame.size() {
                if frame.flags.reliability.is_reliable() {
                    *reliable = true;
                }
                remain -= frame.size();
                frames.push(self.send_medium.pop_front().unwrap());
                // reset penalty
                penalty_sum = 0;
                continue;
            }
            break;
        }

        while let Some(item) = self.send_low.peek() {
            if remain >= item.frame.size() {
                if item.frame.flags.reliability.is_reliable() {
                    *reliable = true;
                }
                remain -= item.frame.size();
                frames.push(self.send_low.pop().unwrap().frame);
                // reset penalty
                penalty_sum = 0;
                continue;
            }
            break;
        }

        debug_assert!(
            self.is_empty() || !frames.is_empty(),
            "every frame size should not exceed MTU"
        );

        if frames.is_empty() {
            *penalty = 0;
            return;
        }

        // calculate the mean of penalty in these frames
        // if there is some frame sending firstly, the penalty is 0
        *penalty = (penalty_sum / frames.len()) as u8;
    }

    pub(crate) fn len(&self) -> usize {
        self.send_high.len() + self.send_medium.len() + self.send_low.len() + self.resend.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pin_project! {
    // OutgoingGuard equips with ACK/NACK flusher and packets buffer and provides
    // resending policies and flush strategies.
    pub(crate) struct OutgoingGuard<F> {
        #[pin]
        frame: F,
        link: SharedLink,
        seq_num_write_index: u24,
        peer: Peer,
        role: Role,
        buf: SendBuffer,
        resend: ResendMap,
    }
}

pub(crate) trait HandleOutgoing: Sized {
    fn handle_outgoing(self, link: SharedLink, peer: Peer, role: Role) -> OutgoingGuard<Self>;
}

impl<F> HandleOutgoing for F
where
    F: for<'a> Sink<(Packet<FramesRef<'a>>, SocketAddr), Error = io::Error>,
{
    fn handle_outgoing(self, link: SharedLink, peer: Peer, role: Role) -> OutgoingGuard<Self> {
        OutgoingGuard {
            frame: self,
            link,
            seq_num_write_index: 0.into(),
            peer,
            role,
            buf: SendBuffer::new(),
            resend: ResendMap::new(
                role,
                peer,
                Box::new(RFC6298Impl::new()),
                Box::new(LegacyCongester::new()),
            ),
        }
    }
}

impl<F> OutgoingGuard<F>
where
    F: for<'a> Sink<(Packet<FramesRef<'a>>, SocketAddr), Error = io::Error>,
{
    fn process_inflight(self: Pin<&mut Self>) {
        let this = self.project();
        this.link.process_ack().for_each(|(ack, received_at)| {
            this.resend.on_ack(ack, received_at);
        });
        this.link.process_nack().for_each(|nack| {
            this.resend.on_nack_into(nack, this.buf);
        });
        this.resend.process_stales(this.buf);
    }

    /// Try to empty the outgoing buffer
    fn try_empty(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.as_mut().process_inflight();

        let mut this = self.project();
        let strategy = cx
            .ext()
            .downcast_ref::<FlushStrategy>()
            .copied()
            .unwrap_or_default();
        let mut ack_cnt = 0;
        let mut nack_cnt = 0;
        let mut pack_cnt = 0;

        while !strategy.check_flushed(this.link, this.buf) {
            // 1. empty the outgoing ack
            ready!(this.frame.as_mut().poll_ready(cx))?;
            if strategy.flush_ack()
                && let Some(ack) = this.link.process_outgoing_ack(this.peer.mtu)
            {
                trace!(
                    "[{}] send ack {ack:?} to {}, total count: {}",
                    this.role,
                    this.peer,
                    ack.total_cnt()
                );
                ack_cnt += ack.total_cnt();
                this.frame.as_mut().start_send((
                    Packet::Connected(connected::Packet::Ack(ack)),
                    this.peer.addr,
                ))?;
            }

            // 2. empty the outgoing nack
            ready!(this.frame.as_mut().poll_ready(cx))?;
            if strategy.flush_nack()
                && let Some(nack) = this.link.process_outgoing_nack(this.peer.mtu)
            {
                trace!(
                    "[{}] send nack {nack:?} to {}, total count: {}",
                    this.role,
                    this.peer,
                    nack.total_cnt()
                );
                nack_cnt += nack.total_cnt();
                this.frame.as_mut().start_send((
                    Packet::Connected(connected::Packet::Nack(nack)),
                    this.peer.addr,
                ))?;
            }

            if !strategy.flush_pack() {
                // skip flushing packets
                continue;
            }

            // 3. empty the unconnected packets
            ready!(this.frame.as_mut().poll_ready(cx))?;
            // only poll one packet each time
            if let Some(packet) = this.link.process_unconnected().next() {
                trace!(
                    "[{}] send unconnected packet to {}, type: {:?}",
                    this.role,
                    this.peer,
                    packet.pack_type()
                );
                this.frame
                    .as_mut()
                    .start_send((Packet::Unconnected(packet), this.peer.addr))?;
                pack_cnt += 1;
            }

            // 4. empty the frame set
            ready!(this.frame.as_mut().poll_ready(cx))?;
            let mut frames = Vec::new();
            let mut reliable = false;
            let mut penalty = 0;
            // When reliable packets reach congestion, they will suppress the transmission of
            // unreliable packets, but the transmission of unreliable packets will not affect the
            // congestion calculation of reliable packets.
            this.buf.pop(
                this.peer.mtu as usize,
                &mut reliable,
                &mut penalty,
                &mut frames,
            );
            if frames.is_empty() {
                continue;
            }
            trace!(
                "[{}] send {} frames to {}, seq_num: {}, reliable: {}, parted: {}, size: {}/{}",
                this.role,
                frames.len(),
                this.peer,
                *this.seq_num_write_index,
                reliable,
                frames[0]
                    .fragment
                    .map(|fragment| format!(
                        "{}[{}/{}]",
                        fragment.parted_id,
                        fragment.parted_index + 1,
                        fragment.parted_size
                    ))
                    .unwrap_or(String::from("false")),
                frames.iter().map(|frame| frame.body.len()).sum::<usize>(),
                frames.iter().map(|frame| frame.size()).sum::<usize>() + FRAME_SET_HEADER_SIZE,
            );
            let frame_set = FrameSet {
                seq_num: *this.seq_num_write_index,
                set: &frames[..],
            };
            this.frame.as_mut().start_send((
                Packet::Connected(connected::Packet::FrameSet(frame_set)),
                this.peer.addr,
            ))?;
            if reliable {
                // keep for resending
                this.resend
                    .record(*this.seq_num_write_index, penalty, frames);
            }
            *this.seq_num_write_index += 1;
            pack_cnt += 1;
        }

        // mark flushed count
        if let Some(strategy_) = cx.ext().downcast_mut::<FlushStrategy>() {
            strategy_.mark_flushed_ack(ack_cnt);
            strategy_.mark_flushed_nack(nack_cnt);
            strategy_.mark_flushed_pack(pack_cnt);
        }

        Poll::Ready(Ok(()))
    }
}

impl<F> Sink<(Priority, Frame)> for OutgoingGuard<F>
where
    F: for<'a> Sink<(Packet<FramesRef<'a>>, SocketAddr), Error = io::Error>,
{
    type Error = io::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.as_mut().process_inflight();
        let inflight = self.resend.inflight;
        let cnwd = self.resend.congester.congestion_window();
        if inflight >= cnwd {
            let wait = self.resend.estimator.rto() / 4;
            trace!(
                "[{}] inflight frames: {}, congestion window: {}, wait {:?} for congestion control",
                self.role,
                inflight,
                cnwd,
                wait
            );
            Reactor::get().insert_timer(rand::random(), Instant::now() + wait, cx.waker());
            return Poll::Pending;
        }
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, item: (Priority, Frame)) -> Result<(), Self::Error> {
        let this = self.project();
        this.buf.send(item);
        // Always success
        Ok(())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().try_empty(cx))?;
        self.project().frame.poll_flush(cx)
    }

    /// Close the outgoing guard, notice that it may resend infinitely if you do not cancel it.
    /// Insure all frames are received by the peer at the point of closing
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // maybe go to sleep, turn on the waking
        self.link.turn_on_waking();
        loop {
            ready!(self.as_mut().try_empty(cx))?;
            debug_assert!(
                self.buf.is_empty()
                    && self.link.unconnected_empty()
                    && self.link.outgoing_ack_empty()
                    && self.link.outgoing_nack_empty()
            );
            ready!(self.as_mut().project().frame.poll_flush(cx))?;
            if self.resend.is_empty() {
                trace!(
                    "[{}] all frames are received by {}, close the outgoing guard",
                    self.role,
                    self.peer,
                );
                break;
            }
            ready!(self.resend.poll_wait(cx));
        }
        // no need to wake up
        self.link.turn_off_waking();
        self.project().frame.poll_close(cx)
    }
}

struct ResendEntry {
    penalty: u8,
    frames: Option<Frames>,
    send_at: Instant,
    expired_at: Instant,
}

struct ResendMap {
    // reliable frames
    reliable: HashMap<u24, ResendEntry>,
    // inflight reliable frames, which equals to the sum of the size of frames in `reliable`
    inflight: usize,
    role: Role,
    peer: Peer,
    last_record_expired_at: Instant,
    estimator: Box<dyn Estimator>,
    congester: Box<dyn CongestionController>,
}

impl ResendMap {
    fn new(
        role: Role,
        peer: Peer,
        estimator: Box<dyn Estimator>,
        congester: Box<dyn CongestionController>,
    ) -> Self {
        Self {
            reliable: HashMap::default(),
            inflight: 0,
            role,
            peer,
            last_record_expired_at: Instant::now(),
            estimator,
            congester,
        }
    }

    fn record(&mut self, seq_num: u24, penalty: u8, frames: Frames) {
        let now = Instant::now();
        let rto = self.estimator.rto();
        let penalty_dur = rto * penalty as u32;
        self.inflight += frames.len();
        self.reliable.insert(
            seq_num,
            ResendEntry {
                penalty,
                frames: Some(frames),
                send_at: now,
                expired_at: now + rto + penalty_dur,
            },
        );
        #[cfg(debug_assertions)]
        self.debug_assert_inflight();
    }

    fn on_ack(&mut self, ack: AckOrNack, received_at: Instant) {
        let mut ack_frames = 0;
        for record in ack.records {
            match record {
                Record::Range(start, end) => {
                    for i in start.to_u32()..=end.to_u32() {
                        if let Some(ResendEntry {
                            send_at,
                            frames: Some(frames),
                            ..
                        }) = self.reliable.remove(&i.into())
                        {
                            let rtt = received_at.saturating_duration_since(send_at);
                            self.estimator.update(rtt);
                            ack_frames += frames.len();
                            self.inflight -= frames.len();
                        }
                    }
                    trace!(
                        "[{}] seq_num {}-{} are ACKed by {}, estimated RTT: {:?}, estimated RTO: {:?}, inflight frames: {}, congestion window before: {}",
                        self.role,
                        start,
                        end,
                        self.peer,
                        self.estimator.rtt(),
                        self.estimator.rto(),
                        self.inflight,
                        self.congester.congestion_window()
                    );
                }
                Record::Single(seq_num) => {
                    if let Some(ResendEntry {
                        send_at,
                        frames: Some(frames),
                        ..
                    }) = self.reliable.remove(&seq_num)
                    {
                        let rtt = received_at.saturating_duration_since(send_at);
                        self.estimator.update(rtt);
                        ack_frames += frames.len();
                        self.inflight -= frames.len();
                        trace!(
                            "[{}] seq_num {seq_num} is ACKed by {}, RTT: {rtt:?}, estimated RTT: {:?}, estimated RTO: {:?}, inflight frames: {}, congestion window before: {}",
                            self.role,
                            self.peer,
                            self.estimator.rtt(),
                            self.estimator.rto(),
                            self.inflight,
                            self.congester.congestion_window()
                        );
                    }
                }
            }
        }
        self.congester.on_ack(ack_frames);

        #[cfg(debug_assertions)]
        self.debug_assert_inflight();
    }

    fn on_nack_into(&mut self, nack: AckOrNack, buf: &mut SendBuffer) {
        let mut nack_frames = 0;
        for record in &nack.records {
            match record {
                Record::Range(start, end) => {
                    for i in start.to_u32()..=end.to_u32() {
                        if let Some(entry) = self.reliable.remove(&i.into()) {
                            let frames = entry.frames.unwrap();
                            self.inflight -= frames.len();
                            nack_frames += frames.len();
                            buf.resend(frames.into_iter().map(|frame| PenaltyFrame {
                                penalty: entry.penalty,
                                frame,
                            }));
                        }
                    }
                }
                Record::Single(seq_num) => {
                    if let Some(entry) = self.reliable.remove(seq_num) {
                        let frames = entry.frames.unwrap();
                        self.inflight -= frames.len();
                        nack_frames += frames.len();
                        buf.resend(frames.into_iter().map(|frame| PenaltyFrame {
                            penalty: entry.penalty,
                            frame,
                        }));
                    }
                }
            }
        }
        self.congester.on_nack(nack_frames);
        trace!(
            "[{}] receive NACKs {nack:?} from {}, nack frames count: {}, inflight frames: {}, congestion window: {}",
            self.role,
            self.peer,
            nack_frames,
            self.inflight,
            self.congester.congestion_window()
        );

        #[cfg(debug_assertions)]
        self.debug_assert_inflight();
    }

    /// `process_stales` collect all stale frames into buffer and remove the expired entries
    fn process_stales(&mut self, buf: &mut SendBuffer) {
        // maximum skip scan RTO, used to avoid network enduring a high RTO and suddenly recovered
        const MAX_SKIP_SCAN_RTO: Duration = Duration::from_secs(3);

        if self.reliable.is_empty() {
            return;
        }

        let now = Instant::now();
        if now < self.last_record_expired_at {
            return;
        }
        // find the first expired_at larger than now
        let mut min_expired_at = now + cmp::min(self.estimator.rto(), MAX_SKIP_SCAN_RTO);
        let len_before = self.reliable.len();
        self.reliable.retain(|_, entry| {
            if entry.expired_at <= now {
                let frames = entry.frames.take().unwrap();
                self.inflight -= frames.len();
                buf.resend(frames.into_iter().map(|frame| PenaltyFrame {
                    penalty: entry.penalty,
                    frame,
                }));
                false
            } else {
                min_expired_at = min_expired_at.min(entry.expired_at);
                true
            }
        });
        debug_assert!(min_expired_at >= now);
        // update the last record expired at
        self.last_record_expired_at = min_expired_at;

        let len = self.reliable.len();
        if len_before > len {
            self.estimator.clear();
            self.congester.on_timeout();
            trace!(
                "[{}] collected {} timeout packets to {}, inflight frames: {}, congestion window: {}",
                self.role,
                len_before - len,
                self.peer,
                self.inflight,
                self.congester.congestion_window()
            );
        }

        #[cfg(debug_assertions)]
        self.debug_assert_inflight();
    }

    fn is_empty(&self) -> bool {
        self.reliable.is_empty()
    }

    #[cfg(debug_assertions)]
    fn debug_assert_inflight(&self) {
        let inflight = self
            .reliable
            .values()
            .map(|entry| entry.frames.as_ref().unwrap().len())
            .sum::<usize>();
        debug_assert_eq!(inflight, self.inflight);
    }

    /// `poll_wait` suspends the task when the resend map needs to wait for the next resend
    fn poll_wait(&self, cx: &mut Context<'_>) -> Poll<()> {
        let expired_at;
        let seq_num;
        let now = Instant::now();
        if let Some((seq, entry)) = self
            .reliable
            .iter()
            .min_by_key(|(_, entry)| entry.expired_at)
            && entry.expired_at > now
        {
            expired_at = entry.expired_at;
            seq_num = *seq;
        } else {
            return Poll::Ready(());
        }
        let key = combine_hashes(self.role.guid(), self.peer.guid);
        trace!(
            "[{}] wait on timer {key} for resend seq_num {} to {} within {:?}",
            self.role,
            seq_num,
            self.peer,
            expired_at - now
        );
        Reactor::get().insert_timer(key, expired_at, cx.waker());
        Poll::Pending
    }
}

#[cfg(test)]
mod test {
    use std::task::{Context, Poll};
    use std::time::{Duration, Instant};

    use bytes::Bytes;

    use super::ResendMap;
    use crate::congestion::legacy::LegacyCongester;
    use crate::estimator::RFC6298Impl;
    use crate::guard::SendBuffer;
    use crate::packet::connected::{AckOrNack, Flags, Frame};
    use crate::utils::tests::{test_trace_log_setup, TestWaker};
    use crate::{Peer, Reliability, Role};

    const TEST_RTO: Duration = Duration::from_millis(520);

    #[test]
    fn test_resend_map_works() {
        let _guard = test_trace_log_setup();
        let mut estimator = Box::new(RFC6298Impl::new());
        for _ in 0..1000 {
            estimator.update(TEST_RTO);
        }
        log::info!("estimator rto: {:?}", estimator.rto());
        let mut map = ResendMap::new(
            Role::test_server(),
            Peer::test(),
            estimator,
            Box::new(LegacyCongester::new()),
        );
        map.record(0.into(), 0, vec![]);
        map.record(1.into(), 0, vec![]);
        map.record(2.into(), 0, vec![]);
        map.record(3.into(), 0, vec![]);
        assert!(!map.is_empty());
        map.on_ack(
            AckOrNack::extend_from([0, 1, 2, 3].into_iter().map(Into::into), 100).unwrap(),
            Instant::now(),
        );
        assert!(map.is_empty());
        assert_eq!(map.inflight, 0);

        map.record(
            4.into(),
            0,
            vec![Frame {
                flags: Flags::new(Reliability::Unreliable, false),
                reliable_frame_index: None,
                seq_frame_index: None,
                ordered: None,
                fragment: None,
                body: Bytes::from_static(b"1"),
            }],
        );
        map.record(
            5.into(),
            0,
            vec![
                Frame {
                    flags: Flags::new(Reliability::Unreliable, false),
                    reliable_frame_index: None,
                    seq_frame_index: None,
                    ordered: None,
                    fragment: None,
                    body: Bytes::from_static(b"2"),
                },
                Frame {
                    flags: Flags::new(Reliability::Unreliable, false),
                    reliable_frame_index: None,
                    seq_frame_index: None,
                    ordered: None,
                    fragment: None,
                    body: Bytes::from_static(b"3"),
                },
            ],
        );
        let mut buffer = SendBuffer::new();
        map.on_nack_into(
            AckOrNack::extend_from([4, 5].into_iter().map(Into::into), 100).unwrap(),
            &mut buffer,
        );
        assert!(map.is_empty());
        assert_eq!(buffer.len(), 3);
        assert_eq!(
            buffer.resend.pop().unwrap().frame.body,
            Bytes::from_static(b"1")
        );
        assert_eq!(
            buffer.resend.pop().unwrap().frame.body,
            Bytes::from_static(b"2")
        );
        assert_eq!(
            buffer.resend.pop().unwrap().frame.body,
            Bytes::from_static(b"3")
        );
    }

    #[test]
    fn test_resend_map_stales() {
        let _guard = test_trace_log_setup();
        let mut estimator = Box::new(RFC6298Impl::new());
        for _ in 0..1000 {
            estimator.update(TEST_RTO);
        }
        log::info!("estimator rto: {:?}", estimator.rto());
        let mut map = ResendMap::new(
            Role::test_server(),
            Peer::test(),
            estimator,
            Box::new(LegacyCongester::new()),
        );
        map.record(0.into(), 0, vec![]);
        map.record(1.into(), 0, vec![]);
        map.record(2.into(), 0, vec![]);
        std::thread::sleep(TEST_RTO + Duration::from_millis(5));
        map.record(3.into(), 0, vec![]);
        let mut buffer = SendBuffer::new();
        map.process_stales(&mut buffer);
        assert_eq!(map.reliable.len(), 1);
    }

    #[tokio::test]
    async fn test_resend_map_poll_wait() {
        let _guard = test_trace_log_setup();
        let mut estimator = Box::new(RFC6298Impl::new());
        for _ in 0..1000 {
            estimator.update(TEST_RTO);
        }
        log::info!("estimator rto: {:?}", estimator.rto());
        let mut map = ResendMap::new(
            Role::test_server(),
            Peer::test(),
            estimator,
            Box::new(LegacyCongester::new()),
        );
        map.record(0.into(), 0, vec![]);
        std::thread::sleep(TEST_RTO + Duration::from_millis(5));
        map.record(1.into(), 0, vec![]);
        map.record(2.into(), 0, vec![]);
        map.record(3.into(), 0, vec![]);

        let mut buffer = SendBuffer::new();
        let res = map.poll_wait(&mut Context::from_waker(&TestWaker::create()));
        assert!(matches!(res, Poll::Ready(_)));

        map.process_stales(&mut buffer);
        assert_eq!(map.reliable.len(), 3);

        std::future::poll_fn(|cx| map.poll_wait(cx)).await;
        map.process_stales(&mut buffer);
        assert!(map.reliable.len() < 3);
    }
}
