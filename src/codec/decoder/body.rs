use std::pin::Pin;
use std::task::{ready, Context, Poll};

use fastrace::local::LocalSpan;
use fastrace::Event;
use futures::Stream;
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

        let Some(frame_set) = ready!(this.frame.as_mut().poll_next(cx)?) else {
            return Poll::Ready(None);
        };

        let span = LocalSpan::enter_with_local_parent("codec.body_decoder")
            .with_properties(|| [("seq_num", frame_set.seq_num.to_string())]);

        match FrameBody::read(frame_set.set.body) {
            Ok(body) => {
                let _ = span.with_property(|| ("frame_type", format!("{:?}", body)));
                Poll::Ready(Some(Ok(body)))
            }
            Err(err) => {
                LocalSpan::add_event(Event::new(err.to_string()));
                Poll::Ready(Some(Err(err)))
            }
        }
    }
}
