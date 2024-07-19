use std::pin::Pin;
use std::task::{Context, Poll};

use futures::{ready, Stream, StreamExt};
use minitrace::local::LocalSpan;
use minitrace::Event;
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::{Frame, FrameBody, FrameSet};

pin_project! {
    pub(crate) struct BodyDecoder<F> {
        #[pin]
        frame: F
    }
}

pub(crate) trait BodyDecoded: Sized {
    fn body_decoded(self) -> BodyDecoder<Self>;
}

impl<F> BodyDecoded for F
where
    F: Stream<Item = Result<FrameSet<Frame>, CodecError>>,
{
    fn body_decoded(self) -> BodyDecoder<Self> {
        BodyDecoder { frame: self }
    }
}

impl<F> Stream for BodyDecoder<F>
where
    F: Stream<Item = Result<FrameSet<Frame>, CodecError>>,
{
    type Item = Result<FrameBody, CodecError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        let Some(frame_set) = ready!(this.frame.poll_next_unpin(cx)?) else {
            return Poll::Ready(None);
        };

        let span = LocalSpan::enter_with_local_parent("codec.body_decoder")
            .with_properties(|| [("frame_seq_num", frame_set.seq_num.to_string())]);

        match FrameBody::read(frame_set.set.body) {
            Ok(body) => {
                let _ = span.with_property(|| ("frame_type", format!("{:?}", body)));
                Poll::Ready(Some(Ok(body)))
            }
            Err(err) => {
                Event::add_to_local_parent(err.to_string(), || []);
                Poll::Ready(Some(Err(err)))
            }
        }
    }
}
