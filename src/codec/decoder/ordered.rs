use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Buf;
use futures::{ready, Stream, StreamExt};
use log::warn;
use minitrace::{Event, Span};
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::{self, Frame, FrameSet};
use crate::utils::u24;

const INITIAL_ORDERING_MAP_CAP: usize = 64;

struct Ordering<B> {
    map: HashMap<u24, FrameSet<Frame<B>>>,
    read: u24,
}

impl<B> Default for Ordering<B> {
    fn default() -> Self {
        Self {
            map: HashMap::with_capacity(INITIAL_ORDERING_MAP_CAP),
            read: 0.into(),
        }
    }
}

pin_project! {
    // Ordering layer, ordered the packets based on ordering_frame_index.
    pub(crate) struct Order<F, B> {
        #[pin]
        frame: F,
        // Max ordered channel that will be used in detailed protocol
        max_channels: usize,
        ordering: Vec<Ordering<B>>,
        span: Option<Span>,
    }
}

pub(crate) trait Ordered<B: Buf>: Sized {
    fn ordered(self, max_channels: usize) -> Order<Self, B>;
}

impl<F, B: Buf> Ordered<B> for F
where
    F: Stream<Item = Result<FrameSet<Frame<B>>, CodecError>>,
{
    fn ordered(self, max_channels: usize) -> Order<Self, B> {
        assert!(
            max_channels < usize::from(u8::MAX),
            "max channels should not be larger than u8::MAX"
        );
        assert!(max_channels > 0, "max_channels > 0");

        Order {
            frame: self,
            max_channels,
            ordering: std::iter::repeat_with(Ordering::default)
                .take(max_channels)
                .collect(),
            span: None,
        }
    }
}

impl<F, B> Stream for Order<F, B>
where
    F: Stream<Item = Result<FrameSet<Frame<B>>, CodecError>>,
{
    type Item = Result<FrameSet<Frame<B>>, CodecError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            // empty each channel in order
            for channel in 0..*this.max_channels {
                let ordering = this
                    .ordering
                    .get_mut(channel)
                    .expect("channel < max_channels");
                // check if we could read next
                if let Some(next) = ordering.map.remove(&ordering.read) {
                    ordering.read += 1;
                    this.span.take();
                    return Poll::Ready(Some(Ok(next)));
                }
            }

            let Some(frame_set) = ready!(this.frame.poll_next_unpin(cx)?) else {
                return Poll::Ready(None);
            };
            this.span.get_or_insert_with(|| {
                Span::enter_with_local_parent("codec.reorder")
                    .with_properties(|| [("frame_seq_num", frame_set.seq_num.to_string())])
            });
            if let Some(connected::Ordered {
                frame_index,
                channel,
            }) = frame_set.set.ordered
            {
                let channel = usize::from(channel);
                if channel >= *this.max_channels {
                    let err = format!("channel {} >= max_channels {}", channel, *this.max_channels);
                    Event::add_to_parent(err.clone(), this.span.as_ref().unwrap(), || []);
                    return Poll::Ready(Some(Err(CodecError::OrderedFrame(err))));
                }
                let ordering = this
                    .ordering
                    .get_mut(channel)
                    .expect("channel < max_channels");
                if frame_index < ordering.read {
                    warn!("ignore old ordered frame index {frame_index}");
                    continue;
                }
                ordering.map.insert(frame_index, frame_set);
                // we cannot read anymore
                continue;
            }
            // the frame set which does not require ordered
            return Poll::Ready(Some(Ok(frame_set)));
        }
    }
}

#[cfg(test)]
mod test {
    use bytes::Bytes;
    use futures::StreamExt;
    use futures_async_stream::stream;

    use super::Ordered;
    use crate::errors::CodecError;
    use crate::packet::connected::{Flags, Frame, FrameSet, Ordered as OrderedFlag};

    fn frame_sets(idx: impl IntoIterator<Item = (u8, u32)>) -> Vec<FrameSet<Frame>> {
        idx.into_iter()
            .map(|(channel, frame_index)| FrameSet {
                seq_num: 0.into(),
                set: Frame {
                    flags: Flags::parse(0b011_11100),
                    reliable_frame_index: None,
                    seq_frame_index: None,
                    ordered: Some(OrderedFlag {
                        frame_index: frame_index.into(),
                        channel,
                    }),
                    fragment: None,
                    body: Bytes::new(),
                },
            })
            .collect()
    }

    #[tokio::test]
    async fn test_ordered_works() {
        let frame = {
            #[stream]
            async {
                for frame_set in
                    frame_sets([(0, 1), (0, 0), (0, 2), (0, 0), (0, 4), (0, 3), (1, 1)])
                {
                    yield frame_set;
                }
            }
        };
        tokio::pin!(frame);
        let mut ordered = frame.map(Ok).ordered(10);
        let cmp_sets = frame_sets([(0, 0), (0, 1), (0, 2), (0, 3), (0, 4)]).into_iter();
        for next in cmp_sets {
            assert_eq!(ordered.next().await.unwrap().unwrap(), next);
        }

        assert!(ordered.next().await.is_none());
    }

    #[tokio::test]
    async fn test_ordered_channel_exceed() {
        let frame = {
            #[stream]
            async {
                for frame_set in frame_sets([(10, 1)]) {
                    yield frame_set;
                }
            }
        };
        tokio::pin!(frame);
        let mut ordered = frame.map(Ok).ordered(10);
        assert!(matches!(
            ordered.next().await.unwrap().unwrap_err(),
            CodecError::OrderedFrame(_)
        ));
    }
}
