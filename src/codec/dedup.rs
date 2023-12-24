use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use futures::{Sink, Stream};
use pin_project_lite::pin_project;
use tracing::debug;

use crate::codec::PollPacket;
use crate::errors::CodecError;
use crate::packet::connected::Uint24le;
use crate::packet::{connected, PackId, Packet};

const USIZE_BITS: usize = std::mem::size_of::<usize>() * 8;
const DEFAULT_BIT_VEC_CAP: usize = 256 * USIZE_BITS;

/// A one-direction bit vector queue
/// It use a ring buffer `VecDeque<usize>` to store bits
///
/// Memory Layout:
///
/// `010000000101_110111011111_000001000000000`
///  ^        ^                ^    ^
///  |        |________________|____|
///  |________|         len    |____|
///     head                    tail
#[derive(Debug, Default)]
struct BitVecQueue {
    store: VecDeque<usize>,
    head: usize,
    tail: usize,
}

impl BitVecQueue {
    fn with_capacity(cap_bits: usize) -> Self {
        Self {
            store: VecDeque::with_capacity(cap_bits / USIZE_BITS),
            head: 0,
            tail: 0,
        }
    }

    fn len(&self) -> usize {
        if self.store.is_empty() {
            return 0;
        }
        (self.store.len() - 1) * USIZE_BITS + self.tail - self.head + 1
    }

    fn get(&self, idx: usize) -> Option<bool> {
        let idx = self.head + idx;
        let index = idx / USIZE_BITS;
        let slot = idx % USIZE_BITS;
        let bits = self.store.get(index)?;
        Some(bits & (1 << (USIZE_BITS - 1 - slot)) != 0)
    }

    fn set(&mut self, idx: usize, v: bool) {
        let idx = self.head + idx;
        let index = idx / USIZE_BITS;
        let slot = idx % USIZE_BITS;
        let Some(bits) = self.store.get_mut(index) else {
            return;
        };
        if v {
            *bits |= 1 << (USIZE_BITS - 1 - slot);
        } else {
            *bits &= !(1 << (USIZE_BITS - 1 - slot));
        }
    }

    fn push(&mut self, v: bool) {
        if self.tail == (USIZE_BITS - 1) || self.store.is_empty() {
            self.tail = 0;
            if v {
                self.store.push_back(1 << (USIZE_BITS - 1));
            } else {
                self.store.push_back(0);
            }
            return;
        }
        self.tail += 1;
        if v {
            let last = self.store.back_mut().unwrap();
            *last |= 1 << (USIZE_BITS - 1 - self.tail);
        }
    }

    fn front(&self) -> Option<bool> {
        let front = self.store.front()?;
        Some(front & (1 << (USIZE_BITS - 1 - self.head)) != 0)
    }

    fn pop(&mut self) {
        let len = self.store.len();
        if self.head == (USIZE_BITS - 1) || len == 0 {
            self.head = 0;
            self.store.pop_front();
            return;
        }
        if len == 1 && self.head == self.tail {
            self.head = 0;
            self.tail = 0;
            self.store.clear();
        }
        self.head += 1;
    }
}

#[derive(Debug, Default)]
struct DuplicateWindow {
    /// First unreceived sequence number, start at 0
    first_unreceived: u32,
    /// Record the received status of sequence numbers start at `first_unreceived`
    /// `true` is received and `false` is unreceived
    received_status: BitVecQueue,
}

impl DuplicateWindow {
    fn with_capacity(cap_bits: usize) -> Self {
        Self {
            first_unreceived: 0,
            received_status: BitVecQueue::with_capacity(cap_bits),
        }
    }

    /// Check whether a sequence number is duplicated
    fn duplicate(&mut self, seq_num: Uint24le) -> bool {
        if seq_num.0 < self.first_unreceived {
            return true;
        }
        let gap = (seq_num.0 - self.first_unreceived) as usize;
        if gap == 0 {
            // received the next sequence number of last received
            // advance the first_unreceived and pop the front of
            // received_status
            self.first_unreceived += 1;
            self.received_status.pop();
        } else if gap < self.received_status.len() {
            // received the sequence number that is recorded in received_status
            // check its status to determine whether it is duplicated
            if self.received_status.get(gap) == Some(true) {
                return true;
            }
            // mark it is received
            self.received_status.set(gap, true);
        } else {
            // received the sequence number that exceed received_status, extend
            // the received_status and record the received_status[gap] as received
            for _ in 0..gap - self.received_status.len() {
                self.received_status.push(false);
            }
            self.received_status.push(true);
        }
        while let Some(true) = self.received_status.front() {
            self.received_status.pop();
            self.first_unreceived += 1;
        }
        false
    }
}

pin_project! {
    // Deduplication layer, abort duplicated packets, should be placed as the first layer
    // on UdpFramed to maximum its effect
    pub(super) struct Dedup<F> {
        #[pin]
        frame: F,
        windows: HashMap<SocketAddr, DuplicateWindow>
    }
}

pub(super) trait Deduplicated: Sized {
    fn deduplicated(self) -> Dedup<Self>;
}

impl<T> Deduplicated for T
where
    T: Stream<Item = Result<(Packet, SocketAddr), CodecError>>
        + Sink<(Packet, SocketAddr), Error = CodecError>,
{
    fn deduplicated(self) -> Dedup<Self> {
        Dedup {
            frame: self,
            windows: HashMap::new(),
        }
    }
}

impl<F> Stream for Dedup<F>
where
    F: Stream<Item = Result<(Packet, SocketAddr), CodecError>>,
{
    type Item = Result<(Packet, SocketAddr), CodecError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        let (packet, addr) = match this.frame.poll_packet(cx) {
            Ok(v) => v,
            Err(poll) => return poll,
        };

        let Packet::Connected(connected::Packet::FrameSet(mut frame_set)) = packet else {
            return Poll::Ready(Some(Ok((packet, addr))));
        };
        let window = this
            .windows
            .entry(addr)
            .or_insert_with(|| DuplicateWindow::with_capacity(DEFAULT_BIT_VEC_CAP));
        frame_set.frames.retain(|frame| {
            let Some(reliable_frame_index) = frame.reliable_frame_index else {
                return true;
            };
            !window.duplicate(reliable_frame_index)
        });
        Poll::Ready(Some(Ok((
            Packet::Connected(connected::Packet::FrameSet(frame_set)),
            addr,
        ))))
    }
}

impl<F> Sink<(Packet, SocketAddr)> for Dedup<F>
where
    F: Sink<(Packet, SocketAddr), Error = CodecError>,
{
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        this.frame.poll_ready(cx)
    }

    fn start_send(
        self: Pin<&mut Self>,
        (packet, addr): (Packet, SocketAddr),
    ) -> Result<(), Self::Error> {
        let this = self.project();
        if let Packet::Connected(connected::Packet::FrameSet(frame_set)) = &packet {
            if matches!(frame_set.inner_pack_id()?, PackId::DisconnectNotification) {
                debug!("disconnect from {}, clean it's dedup window", addr);
                this.windows.remove(&addr);
            }
        };
        this.frame.start_send((packet, addr))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        this.frame.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.poll_flush(cx))?;
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod test {

    use crate::codec::dedup::DuplicateWindow;
    use crate::packet::connected::Uint24le;

    #[test]
    fn test_duplicate_windows_check_ordered() {
        let mut window = DuplicateWindow::default();
        for i in 0..1024 {
            assert!(!window.duplicate(Uint24le(i)));
            assert_eq!(window.first_unreceived, i + 1);
            assert!(window.received_status.len() <= 1);
        }
    }

    #[test]
    fn test_duplicate_windows_check_ordered_dup() {
        let mut window = DuplicateWindow::default();
        for i in 0..512 {
            assert!(!window.duplicate(Uint24le(i)));
            assert_eq!(window.first_unreceived, i + 1);
            assert!(window.received_status.len() <= 1);
        }
        for i in 0..512 {
            assert!(window.duplicate(Uint24le(i)));
        }
    }

    #[test]
    fn test_duplicate_windows_check_gap_dup() {
        let mut window = DuplicateWindow::default();
        assert!(!window.duplicate(Uint24le(0)));
        assert!(!window.duplicate(Uint24le(1)));
        assert!(!window.duplicate(Uint24le(1000)));
        assert!(!window.duplicate(Uint24le(1001)));
        assert!(window.duplicate(Uint24le(1000)));
        assert!(window.duplicate(Uint24le(1001)));
        assert!(!window.duplicate(Uint24le(500)));
        assert!(window.duplicate(Uint24le(500)));
        assert_eq!(window.first_unreceived, 2);
    }

    #[test]
    fn test_duplicate_window_clear_gap_map() {
        let mut window = DuplicateWindow::default();
        for i in (0..256).step_by(2) {
            assert!(!window.duplicate(Uint24le(i)));
        }
        for i in (1..256).step_by(2) {
            assert!(!window.duplicate(Uint24le(i)));
        }
        assert_eq!(window.received_status.len(), 0);
    }
}
