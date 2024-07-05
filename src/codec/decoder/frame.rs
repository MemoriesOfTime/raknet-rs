use std::pin::Pin;
use std::task::{Context, Poll};

use futures::{ready, Stream, StreamExt};
use minitrace::{Event, Span};
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::{Frame, FrameBody, FrameSet};

pin_project! {
    pub(crate) struct FrameDecoder<F> {
        #[pin]
        frame: F
    }
}

pub(crate) trait FrameDecoded: Sized {
    fn frame_decoded(self) -> FrameDecoder<Self>;
}

impl<F> FrameDecoded for F
where
    F: Stream<Item = Result<FrameSet<Frame>, CodecError>>,
{
    fn frame_decoded(self) -> FrameDecoder<Self> {
        FrameDecoder { frame: self }
    }
}

impl<F> Stream for FrameDecoder<F>
where
    F: Stream<Item = Result<FrameSet<Frame>, CodecError>>,
{
    type Item = Result<FrameBody, CodecError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        let Some(frame_set) = ready!(this.frame.poll_next_unpin(cx)?) else {
            return Poll::Ready(None);
        };

        let span = Span::enter_with_local_parent("codec.reframe")
            .with_properties(|| [("frame_seq_num", frame_set.seq_num.to_string())]);

        match FrameBody::read(frame_set.set.body) {
            Ok(body) => {
                let _ = span.with_property(|| ("frame_type", format!("{:?}", body)));
                Poll::Ready(Some(Ok(body)))
            }
            Err(err) => {
                Event::add_to_parent(err.to_string(), &span, || []);
                Poll::Ready(Some(Err(err)))
            }
        }
    }
}
