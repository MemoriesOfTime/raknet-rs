use std::pin::Pin;
use std::task::{ready, Context, Poll};

use fastrace::local::LocalSpan;
use fastrace::{Event, Span};
use futures::Stream;
use log::warn;
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::{self, Frame, FrameSet};
use crate::utils::u24;
use crate::HashMap;

struct Ordering {
    map: HashMap<u24, FrameSet<Frame>>,
    read: u24,
}

impl Default for Ordering {
    fn default() -> Self {
        Self {
            map: HashMap::default(),
            read: 0.into(),
        }
    }
}

pin_project! {
    // Ordering layer, ordered the packets based on ordering_frame_index.
    pub(crate) struct Order<F> {
        #[pin]
        frame: F,
        // Max ordered channel that will be used in detailed protocol
        max_channels: usize,
        ordering: Vec<Ordering>,
        span: Option<Span>,
    }
}

pub(crate) trait Ordered: Sized {
    fn ordered(self, max_channels: usize) -> Order<Self>;
}

impl<F> Ordered for F
where
    F: Stream<Item = Result<FrameSet<Frame>, CodecError>>,
{
    fn ordered(self, max_channels: usize) -> Order<Self> {
        assert!(
            max_channels < 256,
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

impl<F> Stream for Order<F>
where
    F: Stream<Item = Result<FrameSet<Frame>, CodecError>>,
{
    type Item = Result<FrameSet<Frame>, CodecError>;

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

            let Some(frame_set) = ready!(this.frame.as_mut().poll_next(cx)?) else {
                return Poll::Ready(None);
            };
            this.span.get_or_insert_with(|| {
                Span::enter_with_local_parent("codec.reorder").with_properties(|| {
                    [(
                        "pending",
                        this.ordering
                            .iter()
                            .fold(0, |acc, o| acc + o.map.len())
                            .to_string(),
                    )]
                })
            });
            if let Some(connected::Ordered {
                frame_index,
                channel,
            }) = frame_set.set.ordered
            {
                let channel = usize::from(channel);
                if channel >= *this.max_channels {
                    let err = format!("channel {} >= max_channels {}", channel, *this.max_channels);
                    LocalSpan::add_event(Event::new(err.clone()));
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
            this.span.take();
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
