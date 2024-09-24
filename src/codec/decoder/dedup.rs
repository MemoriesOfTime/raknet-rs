use std::pin::Pin;
use std::task::{ready, Context, Poll};

use fastrace::Span;
use futures::Stream;
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::{FrameSet, FramesMut};
use crate::utils::{u24, BitVecQueue};

/// The deduplication window. For each connect, the maximum size is
/// 2 ^ (8 * 3) / 8 / 1024 / 1024 = 2MB.
#[derive(Debug, Default)]
struct DuplicateWindow {
    /// First unreceived sequence number, start at 0
    first_unreceived: u24,
    /// Record the received status of sequence numbers start at `first_unreceived`
    /// `true` is received and `false` is unreceived
    received_status: BitVecQueue,
}

impl DuplicateWindow {
    /// Check whether a sequence number is duplicated
    fn duplicate(&mut self, seq_num: u24) -> bool {
        if seq_num < self.first_unreceived {
            return true;
        }
        let gap = (seq_num - self.first_unreceived).to_usize();
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
                self.received_status.push_back(false);
            }
            self.received_status.push_back(true);
        }
        while let Some(true) = self.received_status.front() {
            self.received_status.pop_front();
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
        window: DuplicateWindow,
        span: Option<Span>,
    }
}

pub(crate) trait Deduplicated: Sized {
    fn deduplicated(self) -> Dedup<Self>;
}

impl<F> Deduplicated for F
where
    F: Stream<Item = Result<FrameSet<FramesMut>, CodecError>>,
{
    fn deduplicated(self) -> Dedup<Self> {
        Dedup {
            frame: self,
            window: DuplicateWindow::default(),
            span: None,
        }
    }
}

impl<F> Stream for Dedup<F>
where
    F: Stream<Item = Result<FrameSet<FramesMut>, CodecError>>,
{
    type Item = Result<FrameSet<FramesMut>, CodecError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            let Some(mut frame_set) = ready!(this.frame.as_mut().poll_next(cx)?) else {
                return Poll::Ready(None);
            };
            this.span.get_or_insert_with(|| {
                Span::enter_with_local_parent("codec.deduplication").with_properties(|| {
                    [("wnd_size", this.window.received_status.len().to_string())]
                })
            });
            frame_set.set.retain(|frame| {
                let Some(reliable_frame_index) = frame.reliable_frame_index else {
                    // no reliable_frame_index, just pass
                    return true;
                };
                !this.window.duplicate(reliable_frame_index)
            });
            if !frame_set.set.is_empty() {
                this.span.take();
                return Poll::Ready(Some(Ok(frame_set)));
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::ops::Sub;

    use bytes::BytesMut;
    use futures::StreamExt;
    use futures_async_stream::stream;
    use indexmap::IndexSet;

    use super::*;
    use crate::packet::connected::{Flags, Frame, FrameSet};

    #[test]
    fn test_duplicate_windows_check_ordered() {
        let mut window = DuplicateWindow::default();
        for i in 0..1024 {
            assert!(!window.duplicate(i.into()));
            assert_eq!(window.first_unreceived.to_u32(), i + 1);
            assert!(window.received_status.len() <= 1);
        }
    }

    #[test]
    fn test_duplicate_windows_check_ordered_dup() {
        let mut window = DuplicateWindow::default();
        for i in 0..512 {
            assert!(!window.duplicate(i.into()));
            assert_eq!(window.first_unreceived.to_u32(), i + 1);
            assert!(window.received_status.len() <= 1);
        }
        for i in 0..512 {
            assert!(window.duplicate(i.into()));
        }
    }

    #[test]
    fn test_duplicate_windows_check_gap_dup() {
        let mut window = DuplicateWindow::default();
        assert!(!window.duplicate(0.into()));
        assert!(!window.duplicate(1.into()));
        assert!(!window.duplicate(1000.into()));
        assert!(!window.duplicate(1001.into()));
        assert!(window.duplicate(1000.into()));
        assert!(window.duplicate(1001.into()));
        assert!(!window.duplicate(500.into()));
        assert!(window.duplicate(500.into()));
        assert_eq!(window.first_unreceived.to_u32(), 2);
    }

    #[test]
    fn test_duplicate_window_clear_gap_map() {
        let mut window = DuplicateWindow::default();
        for i in (0..256).step_by(2) {
            assert!(!window.duplicate(i.into()));
        }
        for i in (1..256).step_by(2) {
            assert!(!window.duplicate(i.into()));
        }
        assert_eq!(window.received_status.len(), 0);
    }

    fn frame_set(idx: impl IntoIterator<Item = u32>) -> FrameSet<FramesMut> {
        FrameSet {
            seq_num: 0.into(),
            set: idx
                .into_iter()
                .map(|i| Frame {
                    flags: Flags::parse(0b011_11100),
                    reliable_frame_index: Some(i.into()),
                    seq_frame_index: None,
                    ordered: None,
                    fragment: None,
                    body: BytesMut::new(),
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
        let mut dedup = frame.map(Ok).deduplicated();

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
    async fn test_dedup_same() {
        let frame = {
            #[stream]
            async {
                yield frame_set([0, 1, 2, 3]);
                yield frame_set([0, 1, 2, 3]);
            }
        };
        tokio::pin!(frame);
        let mut dedup = frame.map(Ok).deduplicated();
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
        let mut dedup = frame.map(Ok).deduplicated();
        assert_eq!(dedup.next().await.unwrap().unwrap(), frame_set(idx1_set));

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
