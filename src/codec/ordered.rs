use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::{Sink, Stream};
use pin_project_lite::pin_project;

use super::PollPacket;
use crate::errors::CodecError;
use crate::packet::connected::FrameSet;
use crate::packet::{connected, Packet};

pin_project! {
    pub(super) struct Order<F> {
        #[pin]
        frame: F,
        // Max ordered channel that will be used in detailed protocol
        max_channels: u8,
    }
}

pub(super) trait Ordered: Sized {
    fn ordered(self, max_channels: u8) -> Order<Self>;
}

impl<T> Ordered for T
where
    T: Stream<Item = Result<(Packet, SocketAddr), CodecError>>
        + Sink<(Packet, SocketAddr), Error = CodecError>,
{
    fn ordered(self, max_channels: u8) -> Order<Self> {
        Order {
            frame: self,
            max_channels,
        }
    }
}

impl<F> Stream for Order<F>
where
    F: Stream<Item = Result<(Packet, SocketAddr), CodecError>>,
{
    type Item = Result<(Packet, SocketAddr), CodecError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        let (packet, addr) = match this.frame.as_mut().poll_packet(cx) {
            Ok(v) => v,
            Err(poll) => return poll,
        };

        let Packet::Connected(connected::Packet::FrameSet(frame_set)) = packet else {
            return Poll::Ready(Some(Ok((packet, addr))));
        };
        if frame_set.frames.is_empty() {
            return Poll::Ready(Some(Ok((
                Packet::Connected(connected::Packet::FrameSet(frame_set)),
                addr,
            ))));
        }
        for frame in frame_set.frames {
            if !frame.flags.reliability().is_ordered() {
                // just return if it does not require ordered
                return Poll::Ready(Some(Ok((
                    Packet::Connected(connected::Packet::FrameSet(FrameSet {
                        frames: vec![frame],
                        ..frame_set
                    })),
                    addr,
                ))));
            }
            // TODO: order layer
        }

        Poll::Pending
    }
}
