use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use flume::Sender;
use futures::{ready, Stream, StreamExt};
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::{FrameSet, Frames, Uint24le};

const USIZE_BITS: usize = std::mem::size_of::<usize>() * 8;
const DEFAULT_BIT_VEC_QUEUE_CAP: usize = 256 * USIZE_BITS;

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
#[derive(Debug, Clone)]
struct BitVecQueue {
    store: VecDeque<usize>,
    head: usize,
    tail: usize,
}

impl Default for BitVecQueue {
    #[inline]
    fn default() -> Self {
        Self::with_capacity(DEFAULT_BIT_VEC_QUEUE_CAP)
    }
}

impl BitVecQueue {
    /// New with a capacity (in bits)
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
        if index == self.store.len() - 1 && slot > self.tail {
            return None;
        }
        let bits = self.store.get(index)?;
        Some(bits & (1 << (USIZE_BITS - 1 - slot)) != 0)
    }

    fn set(&mut self, idx: usize, v: bool) {
        let idx = self.head + idx;
        let index = idx / USIZE_BITS;
        let slot = idx % USIZE_BITS;
        if index == self.store.len() - 1 && slot > self.tail {
            return;
        }
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
        if len == 0 {
            return;
        }
        if len == 1 && self.head == self.tail {
            self.clear();
            return;
        }
        if self.head == (USIZE_BITS - 1) {
            self.head = 0;
            let _ig = self.store.pop_front();
            return;
        }
        self.head += 1;
    }

    /// Clear the bit queue
    fn clear(&mut self) {
        self.head = 0;
        self.tail = 0;
        self.store.clear();
    }
}

/// The deduplication window. For each connect, the maximum size is
/// 2 ^ (8 * 3) / 8 / 1024 / 1024 = 2MB.
#[derive(Debug, Default)]
struct DuplicateWindow {
    /// First unreceived sequence number, start at 0
    first_unreceived: u32,
    /// Record the received status of sequence numbers start at `first_unreceived`
    /// `true` is received and `false` is unreceived
    received_status: BitVecQueue,
}

impl DuplicateWindow {
    /// Check whether a sequence number is duplicated
    fn duplicate(&mut self, seq_num: Uint24le) -> bool {
        if seq_num.0 < self.first_unreceived {
            return true;
        }
        let gap = (seq_num.0 - self.first_unreceived) as usize;
        if gap < self.received_status.len() {
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
    /// Deduplication layer, abort duplicated packets, should be placed as the first layer
    /// on UdpFramed to maximum its effect
    pub(crate) struct Dedup<F> {
        #[pin]
        frame: F,
        // Limit the maximum reliable_frame_index gap for a connection. 0 means no limit.
        max_gap: usize,
        window: DuplicateWindow,
        outgoing_ack_tx: Sender<u32>,
    }
}

pub(crate) trait Deduplicated: Sized {
    fn deduplicated(self, max_gap: usize, outgoing_ack_tx: Sender<u32>) -> Dedup<Self>;
}

impl<F, B> Deduplicated for F
where
    F: Stream<Item = Result<FrameSet<Frames<B>>, CodecError>>,
{
    fn deduplicated(self, max_gap: usize, outgoing_ack_tx: Sender<u32>) -> Dedup<Self> {
        Dedup {
            frame: self,
            max_gap,
            window: DuplicateWindow::default(),
            outgoing_ack_tx,
        }
    }
}

impl<F, B> Stream for Dedup<F>
where
    F: Stream<Item = Result<FrameSet<Frames<B>>, CodecError>>,
{
    type Item = Result<FrameSet<Frames<B>>, CodecError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            let Some(mut frame_set) = ready!(this.frame.poll_next_unpin(cx)?) else {
                return Poll::Ready(None);
            };
            if *this.max_gap != 0 && this.window.received_status.len() > *this.max_gap {
                return Poll::Ready(Some(Err(CodecError::DedupExceed(
                    *this.max_gap,
                    this.window.received_status.len(),
                ))));
            }
            frame_set.set.retain(|frame| {
                let Some(reliable_frame_index) = frame.reliable_frame_index else {
                    return true;
                };
                !this.window.duplicate(reliable_frame_index)
            });
            if !frame_set.set.is_empty() {
                return Poll::Ready(Some(Ok(frame_set)));
            } else {
                // all duplicated, send ack back
                this.outgoing_ack_tx
                    .send(frame_set.seq_num.0)
                    .expect("outgoing_ack_rx never drops");
            }
        }
    }
}

#[cfg(test)]
mod test {

    use std::ops::Sub;

    use bytes::Bytes;
    use futures::StreamExt;
    use futures_async_stream::stream;
    use indexmap::IndexSet;

    use super::*;
    use crate::errors::CodecError;
    use crate::packet::connected::{Flags, Frame, FrameSet, Uint24le};

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

    fn frame_set(idx: impl IntoIterator<Item = u32>) -> FrameSet<Frames<Bytes>> {
        FrameSet {
            seq_num: Uint24le(0),
            set: idx
                .into_iter()
                .map(|i| Frame {
                    flags: Flags::parse(0b011_11100),
                    reliable_frame_index: Some(Uint24le(i)),
                    seq_frame_index: None,
                    ordered: None,
                    fragment: None,
                    body: Bytes::new(),
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn test_dedup_works() {
        let frame = {
            #[stream]
            async {
                yield frame_set(0..64);
                yield frame_set(0..64); // duplicated
                yield frame_set([65, 66, 68, 69]);
                yield frame_set([67, 68]);
                yield frame_set([71, 71, 72]);
                yield frame_set([70]);
            }
        };
        tokio::pin!(frame);
        let (outgoing_ack_tx, _rx) = flume::unbounded();
        let mut dedup = Dedup {
            frame: frame.map(Ok),
            max_gap: 100,
            window: DuplicateWindow::default(),
            outgoing_ack_tx,
        };

        assert_eq!(dedup.next().await.unwrap().unwrap(), frame_set(0..64));
        assert_eq!(
            dedup.next().await.unwrap().unwrap(),
            frame_set([65, 66, 68, 69])
        );
        assert_eq!(dedup.next().await.unwrap().unwrap(), frame_set([67]));
        assert_eq!(dedup.next().await.unwrap().unwrap(), frame_set([71, 72]));
        assert_eq!(dedup.next().await.unwrap().unwrap(), frame_set([70]));
    }

    #[tokio::test]
    async fn test_dedup_exceed() {
        let frame = {
            #[stream]
            async {
                yield frame_set([0]);
                yield frame_set([101]);
                yield frame_set([102]);
            }
        };
        tokio::pin!(frame);
        let (outgoing_ack_tx, _rx) = flume::unbounded();
        let mut dedup = Dedup {
            frame: frame.map(Ok),
            max_gap: 100,
            window: DuplicateWindow::default(),
            outgoing_ack_tx,
        };
        assert_eq!(dedup.next().await.unwrap().unwrap(), frame_set([0]));
        assert_eq!(dedup.next().await.unwrap().unwrap(), frame_set([101]));
        assert!(matches!(
            dedup.next().await.unwrap(),
            Err(CodecError::DedupExceed(..))
        ));
    }

    #[tokio::test]
    async fn test_dedup_same() {
        let frame = {
            #[stream]
            async {
                yield frame_set([0, 1, 2, 3]);
                yield frame_set([0, 1, 2, 3]);
            }
        };
        tokio::pin!(frame);
        let (outgoing_ack_tx, _rx) = flume::unbounded();
        let mut dedup = Dedup {
            frame: frame.map(Ok),
            max_gap: 100,
            window: DuplicateWindow::default(),
            outgoing_ack_tx,
        };
        assert_eq!(
            dedup.next().await.unwrap().unwrap(),
            frame_set([0, 1, 2, 3])
        );
        assert!(dedup.next().await.is_none());
    }

    async fn test_dedup_fuzzing_with_scale(scale: usize) {
        let idx1 = std::iter::repeat_with(rand::random::<u32>)
            .map(|i| i % scale as u32)
            .take(scale)
            .collect::<Vec<_>>();
        let idx2 = std::iter::repeat_with(rand::random::<u32>)
            .map(|i| i % scale as u32)
            .take(scale)
            .collect::<Vec<_>>();

        let idx1_set: IndexSet<u32> = idx1.clone().into_iter().collect();
        let idx2_set: IndexSet<u32> = idx2.clone().into_iter().collect();
        let diff = idx2_set.sub(&idx1_set);

        let frame = {
            #[stream]
            async {
                yield frame_set(idx1);
                yield frame_set(idx2);
            }
        };
        tokio::pin!(frame);
        let (outgoing_ack_tx, _rx) = flume::unbounded();
        let mut dedup = Dedup {
            frame: frame.map(Ok),
            max_gap: scale,
            window: DuplicateWindow::default(),
            outgoing_ack_tx,
        };
        assert_eq!(
            dedup.next().await.unwrap().unwrap(),
            frame_set(idx1_set.clone())
        );

        if diff.is_empty() {
            assert!(dedup.next().await.is_none());
        } else {
            assert_eq!(dedup.next().await.unwrap().unwrap(), frame_set(diff));
        }
    }

    #[tokio::test]
    async fn test_dedup_fuzzing_with_scale_10() {
        test_dedup_fuzzing_with_scale(10).await;
    }

    #[tokio::test]
    async fn test_dedup_fuzzing_with_scale_100() {
        test_dedup_fuzzing_with_scale(100).await;
    }

    #[tokio::test]
    async fn test_dedup_fuzzing_with_scale_1000() {
        test_dedup_fuzzing_with_scale(1000).await;
    }

    #[tokio::test]
    async fn test_dedup_fuzzing_with_scale_10000() {
        test_dedup_fuzzing_with_scale(10000).await;
    }

    #[tokio::test]
    async fn test_dedup_fuzzing_with_scale_100000() {
        test_dedup_fuzzing_with_scale(100000).await;
    }
}
