use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::time::Instant;

use futures::Sink;
use log::trace;
use pin_project_lite::pin_project;

use crate::estimator::{Estimator, RFC6298Impl};
use crate::link::SharedLink;
use crate::opts::FlushStrategy;
use crate::packet::connected::{self, AckOrNack, Frame, FrameSet, Frames, FramesRef, Record};
use crate::packet::{Packet, FRAME_SET_HEADER_SIZE};
use crate::utils::{u24, ConnId, Reactor};
use crate::{Peer, Role};

pin_project! {
    // OutgoingGuard equips with ACK/NACK flusher and packets buffer and provides
    // resending policies and flush strategies.
    pub(crate) struct OutgoingGuard<F> {
        #[pin]
        frame: F,
        link: SharedLink,
        seq_num_write_index: u24,
        buf: VecDeque<Frame>,
        peer: Peer,
        role: Role,
        cap: usize,
        resend: ResendMap,
    }
}

pub(crate) trait HandleOutgoing: Sized {
    fn handle_outgoing(
        self,
        link: SharedLink,
        cap: usize,
        peer: Peer,
        role: Role,
    ) -> OutgoingGuard<Self>;
}

impl<F> HandleOutgoing for F
where
    F: for<'a> Sink<(Packet<FramesRef<'a>>, SocketAddr), Error = io::Error>,
{
    fn handle_outgoing(
        self,
        link: SharedLink,
        cap: usize,
        peer: Peer,
        role: Role,
    ) -> OutgoingGuard<Self> {
        assert!(cap > 0, "cap must larger than 0");
        OutgoingGuard {
            frame: self,
            link,
            seq_num_write_index: 0.into(),
            buf: VecDeque::with_capacity(cap),
            peer,
            role,
            cap,
            resend: ResendMap::new(role, peer, Box::new(RFC6298Impl::new())),
        }
    }
}

impl<F> OutgoingGuard<F>
where
    F: for<'a> Sink<(Packet<FramesRef<'a>>, SocketAddr), Error = io::Error>,
{
    /// Try to empty the outgoing buffer
    fn try_empty(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let mut this = self.project();

        this.link
            .process_ack()
            .for_each(|(ack, received_at)| this.resend.on_ack(ack, received_at));
        this.link
            .process_nack()
            .for_each(|nack| this.resend.on_nack_into(nack, this.buf));
        this.resend.process_stales(this.buf);
        let strategy = cx
            .ext()
            .downcast_ref::<FlushStrategy>()
            .copied()
            .unwrap_or_default();
        let mut ack_cnt = 0;
        let mut nack_cnt = 0;
        let mut pack_cnt = 0;

        while !strategy.check_flushed(this.link, this.buf) {
            // 1st. empty the nack
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

            // 2nd. empty the ack
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

            if !strategy.flush_pack() {
                // skip flushing packets
                continue;
            }

            // 3rd. empty the unconnected packets
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

            // 4th. empty the frame set
            ready!(this.frame.as_mut().poll_ready(cx))?;
            let mut frames = Vec::with_capacity(this.buf.len());
            let mut reliable = false;
            let mut remain = this.peer.mtu as usize - FRAME_SET_HEADER_SIZE;
            while let Some(frame) = this.buf.back() {
                if remain >= frame.size() {
                    if frame.flags.reliability.is_reliable() {
                        reliable = true;
                    }
                    remain -= frame.size();
                    trace!(
                        "[{}] send frame to {}, seq_num: {}, reliable: {}, first byte: 0x{:02x}, size: {}",
                        this.role,
                        this.peer,
                        *this.seq_num_write_index,
                        reliable,
                        frame.body[0],
                        frame.size()
                    );
                    frames.push(this.buf.pop_back().unwrap());
                    continue;
                }
                break;
            }
            debug_assert!(
                this.buf.is_empty() || !frames.is_empty(),
                "every frame size should not exceed MTU"
            );
            if !frames.is_empty() {
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
                    this.resend.record(*this.seq_num_write_index, frames);
                }
                *this.seq_num_write_index += 1;
                pack_cnt += 1;
            }
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

impl<F> Sink<Frame> for OutgoingGuard<F>
where
    F: for<'a> Sink<(Packet<FramesRef<'a>>, SocketAddr), Error = io::Error>,
{
    type Error = io::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let upstream = self.as_mut().try_empty(cx)?;

        if self.buf.len() >= self.cap {
            debug_assert!(
                upstream == Poll::Pending,
                "OutgoingGuard::try_empty returns Ready but buffer still remains!"
            );
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn start_send(self: Pin<&mut Self>, frame: Frame) -> Result<(), Self::Error> {
        let this = self.project();
        this.buf.push_front(frame);
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
    frames: Option<Frames>,
    send_at: Instant,
    expired_at: Instant,
}

struct ResendMap {
    map: HashMap<u24, ResendEntry>,
    role: Role,
    peer: Peer,
    last_record_expired_at: Instant,
    estimator: Box<dyn Estimator + Send>,
}

impl ResendMap {
    fn new(role: Role, peer: Peer, estimator: Box<dyn Estimator + Send>) -> Self {
        Self {
            map: HashMap::new(),
            role,
            peer,
            last_record_expired_at: Instant::now(),
            estimator,
        }
    }

    fn record(&mut self, seq_num: u24, frames: Frames) {
        let now = Instant::now();
        self.map.insert(
            seq_num,
            ResendEntry {
                frames: Some(frames),
                send_at: now,
                expired_at: now + self.estimator.rto(),
            },
        );
    }

    fn on_ack(&mut self, ack: AckOrNack, received_at: Instant) {
        for record in ack.records {
            match record {
                Record::Range(start, end) => {
                    for i in start.to_u32()..=end.to_u32() {
                        if let Some(ResendEntry { send_at, .. }) = self.map.remove(&i.into()) {
                            let rtt = received_at.saturating_duration_since(send_at);
                            self.estimator.update(rtt);
                            trace!(
                                "[{}] seq_num {i} is ACKed by {}, RTT: {rtt:?}, estimated RTO: {:?}",
                                self.role,
                                self.peer,
                                self.estimator.rto()
                            );
                        }
                    }
                }
                Record::Single(seq_num) => {
                    if let Some(ResendEntry { send_at, .. }) = self.map.remove(&seq_num) {
                        let rtt = received_at.saturating_duration_since(send_at);
                        self.estimator.update(rtt);
                        trace!(
                            "[{}] seq_num {seq_num} is ACKed by {}, RTT: {rtt:?}, estimated RTO: {:?}",
                            self.role,
                            self.peer,
                            self.estimator.rto()
                        );
                    }
                }
            }
        }
    }

    fn on_nack_into(&mut self, nack: AckOrNack, buffer: &mut VecDeque<Frame>) {
        trace!("[{}] receive NACKs {nack:?} from {}", self.role, self.peer);
        for record in nack.records {
            match record {
                Record::Range(start, end) => {
                    for i in start.to_u32()..=end.to_u32() {
                        if let Some(entry) = self.map.remove(&i.into()) {
                            buffer.extend(entry.frames.unwrap());
                        }
                    }
                }
                Record::Single(seq_num) => {
                    if let Some(entry) = self.map.remove(&seq_num) {
                        buffer.extend(entry.frames.unwrap());
                    }
                }
            }
        }
    }

    /// `process_stales` collect all stale frames into buffer and remove the expired entries
    fn process_stales(&mut self, buffer: &mut VecDeque<Frame>) {
        if self.map.is_empty() {
            return;
        }

        let now = Instant::now();
        if now < self.last_record_expired_at {
            trace!(
                "[{}] skip scanning the resend map, last record expired at {:?}",
                self.role,
                self.last_record_expired_at - now
            );
            return;
        }
        // find the first expired_at larger than now
        let mut min_expired_at = now + self.estimator.rto();
        let len_before = self.map.len();
        self.map.retain(|_, entry| {
            if entry.expired_at <= now {
                buffer.extend(entry.frames.take().unwrap());
                false
            } else {
                min_expired_at = min_expired_at.min(entry.expired_at);
                true
            }
        });
        debug_assert!(min_expired_at > now);
        // update the last record expired at
        self.last_record_expired_at = min_expired_at;

        let len = self.map.len();
        if len_before > len {
            // clear the estimator if detected packet loss
            self.estimator.clear();
        }
        trace!(
            "[{}]: resend {} stales, {} entries remains",
            self.role,
            len_before - len,
            len,
        );
    }

    fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// `poll_wait` suspends the task when the resend map needs to wait for the next resend
    fn poll_wait(&self, cx: &mut Context<'_>) -> Poll<()> {
        let expired_at;
        let seq_num;
        let now = Instant::now();
        if let Some((seq, entry)) = self.map.iter().min_by_key(|(_, entry)| entry.expired_at)
            && entry.expired_at > now
        {
            expired_at = entry.expired_at;
            seq_num = *seq;
        } else {
            return Poll::Ready(());
        }
        let c_id = ConnId::new(self.role.guid(), self.peer.guid);
        trace!(
            "[{}]: wait on {c_id:?} for resend seq_num {} to {} within {:?}",
            self.role,
            seq_num,
            self.peer,
            expired_at - now
        );
        Reactor::get().insert_timer(c_id, expired_at, cx.waker());
        Poll::Pending
    }
}

#[cfg(test)]
mod test {
    use std::collections::VecDeque;
    use std::task::{Context, Poll};
    use std::time::{Duration, Instant};

    use bytes::Bytes;

    use super::ResendMap;
    use crate::estimator::RFC6298Impl;
    use crate::packet::connected::{AckOrNack, Flags, Frame};
    use crate::utils::tests::{test_trace_log_setup, TestWaker};
    use crate::{Peer, Reliability, Role};

    const TEST_RTO: Duration = Duration::from_millis(1200);

    #[test]
    fn test_resend_map_works() {
        let mut map = ResendMap::new(
            Role::test_server(),
            Peer::test(),
            Box::new(RFC6298Impl::new()),
        );
        map.record(0.into(), vec![]);
        map.record(1.into(), vec![]);
        map.record(2.into(), vec![]);
        map.record(3.into(), vec![]);
        assert!(!map.is_empty());
        map.on_ack(
            AckOrNack::extend_from([0, 1, 2, 3].into_iter().map(Into::into), 100).unwrap(),
            Instant::now(),
        );
        assert!(map.is_empty());

        map.record(
            4.into(),
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
        let mut buffer = VecDeque::default();
        map.on_nack_into(
            AckOrNack::extend_from([4, 5].into_iter().map(Into::into), 100).unwrap(),
            &mut buffer,
        );
        assert!(map.is_empty());
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.pop_front().unwrap().body, Bytes::from_static(b"1"));
        assert_eq!(buffer.pop_front().unwrap().body, Bytes::from_static(b"2"));
        assert_eq!(buffer.pop_front().unwrap().body, Bytes::from_static(b"3"));
    }

    #[test]
    fn test_resend_map_stales() {
        let mut map = ResendMap::new(
            Role::test_server(),
            Peer::test(),
            Box::new(RFC6298Impl::new()),
        );
        map.record(0.into(), vec![]);
        map.record(1.into(), vec![]);
        map.record(2.into(), vec![]);
        std::thread::sleep(TEST_RTO);
        map.record(3.into(), vec![]);
        let mut buffer = VecDeque::default();
        map.process_stales(&mut buffer);
        assert_eq!(map.map.len(), 1);
    }

    #[tokio::test]
    async fn test_resend_map_poll_wait() {
        let _guard = test_trace_log_setup();

        let mut map = ResendMap::new(
            Role::test_server(),
            Peer::test(),
            Box::new(RFC6298Impl::new()),
        );
        map.record(0.into(), vec![]);
        std::thread::sleep(TEST_RTO);
        map.record(1.into(), vec![]);
        map.record(2.into(), vec![]);
        map.record(3.into(), vec![]);

        let mut buffer = VecDeque::default();

        let res = map.poll_wait(&mut Context::from_waker(&TestWaker::create()));
        assert!(matches!(res, Poll::Ready(_)));

        map.process_stales(&mut buffer);
        assert_eq!(map.map.len(), 3);

        std::future::poll_fn(|cx| map.poll_wait(cx)).await;
        map.process_stales(&mut buffer);
        assert!(map.map.len() < 3);
    }
}
