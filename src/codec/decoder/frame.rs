use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::{ready, Stream, StreamExt};
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::{self, Frame, FrameBody, FrameSet};

pin_project! {
    pub(crate) struct FrameDecoder<F> {
        #[pin]
        frame: F
    }
}

pub(crate) trait FrameDecoded: Sized {
    fn frame_decoded(self) -> FrameDecoder<Self>;
}

impl<F> FrameDecoded for F {
    fn frame_decoded(self) -> FrameDecoder<Self> {
        FrameDecoder { frame: self }
    }
}

impl<F> Stream for FrameDecoder<F>
where
    F: Stream<Item = Result<connected::Packet<Bytes>, CodecError>>,
{
    type Item = Result<connected::Packet<FrameBody>, CodecError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        let Some(packet) = ready!(this.frame.poll_next_unpin(cx)?) else {
            return Poll::Ready(None);
        };

        let frame_set = match packet {
            connected::Packet::FrameSet(frame_set) => frame_set,
            connected::Packet::Ack(ack) => {
                return Poll::Ready(Some(Ok(connected::Packet::Ack(ack))))
            }
            connected::Packet::Nack(nack) => {
                return Poll::Ready(Some(Ok(connected::Packet::Nack(nack))))
            }
        };

        let frames = frame_set
            .frames
            .into_iter()
            .map(|frame| {
                Ok::<_, CodecError>(Frame {
                    body: FrameBody::read(frame.body)?,
                    ..frame
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        Poll::Ready(Some(Ok(connected::Packet::FrameSet(FrameSet {
            seq_num: frame_set.seq_num,
            frames,
        }))))
    }
}
