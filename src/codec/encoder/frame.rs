use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::BytesMut;
use futures::Sink;
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::link::SharedLink;
use crate::packet::connected::{FrameBody, Reliability};
use crate::Message;

pin_project! {
    pub(crate) struct FrameEncoder<F> {
        #[pin]
        frame: F,
        link: SharedLink,
    }
}

pub(crate) trait FrameEncoded: Sized {
    fn frame_encoded(self, link: SharedLink) -> FrameEncoder<Self>;
}

impl<F> FrameEncoded for F
where
    F: Sink<Message, Error = CodecError>,
{
    fn frame_encoded(self, link: SharedLink) -> FrameEncoder<Self> {
        FrameEncoder { frame: self, link }
    }
}

#[inline(always)]
fn encode(body: FrameBody) -> Message {
    const DEFAULT_FRAME_BODY_ORDERED_CHANNEL: u8 = 0;

    let reliability = match body {
        FrameBody::ConnectedPing { .. } => Reliability::Unreliable,
        FrameBody::ConnectedPong { .. } => Reliability::Unreliable,
        FrameBody::ConnectionRequest { .. } => Reliability::ReliableOrdered,
        FrameBody::ConnectionRequestAccepted { .. } => Reliability::Reliable,
        FrameBody::NewIncomingConnection { .. } => Reliability::ReliableOrdered,
        FrameBody::DisconnectNotification => Reliability::Reliable,
        FrameBody::DetectLostConnections => Reliability::Reliable,
        FrameBody::User(_) => {
            panic!("you should not send user packet into FrameEncoder, please send `Message`")
        }
    };
    let mut data = BytesMut::new();
    body.write(&mut data);
    Message::new(
        reliability,
        DEFAULT_FRAME_BODY_ORDERED_CHANNEL,
        data.freeze(),
    )
}

impl<F> FrameEncoder<F>
where
    F: Sink<Message, Error = CodecError>,
{
    /// Empty the link buffer all the frame body, insure the frame is ready to send
    pub(crate) fn poll_empty(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), CodecError>> {
        let mut this = self.project();
        if this.link.frame_body_empty() {
            return Poll::Ready(Ok(()));
        }

        ready!(this.frame.as_mut().poll_ready(cx))?;

        // frame is now ready to send
        for body in this.link.process_frame_body() {
            this.frame.as_mut().start_send(encode(body))?;
            // ready for next frame
            ready!(this.frame.as_mut().poll_ready(cx))?;
        }
        Poll::Ready(Ok(()))
    }
}

impl<F> Sink<Message> for FrameEncoder<F>
where
    F: Sink<Message, Error = CodecError>,
{
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<FrameBody>::poll_ready(self, cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        // skip encode
        self.project().frame.start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<FrameBody>::poll_flush(self, cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<FrameBody>::poll_close(self, cx)
    }
}

impl<F> Sink<FrameBody> for FrameEncoder<F>
where
    F: Sink<Message, Error = CodecError>,
{
    type Error = CodecError;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().poll_empty(cx))?;
        // there is literally no buffer
        debug_assert!(self.link.frame_body_empty());
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, body: FrameBody) -> Result<(), Self::Error> {
        self.project().frame.start_send(encode(body))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().poll_empty(cx))?;
        debug_assert!(self.link.frame_body_empty());
        self.project().frame.poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().poll_empty(cx))?;
        debug_assert!(self.link.frame_body_empty());
        self.project().frame.poll_close(cx)
    }
}
