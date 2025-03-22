use std::io;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::BytesMut;
use futures::Sink;
use pin_project_lite::pin_project;

use crate::link::SharedLink;
use crate::packet::connected::FrameBody;
use crate::{Message, Priority, Reliability};

pin_project! {
    // BodyEncoder encodes internal frame body into Message
    pub(crate) struct BodyEncoder<F> {
        #[pin]
        frame: F,
        link: SharedLink,
    }
}

pub(crate) trait BodyEncoded: Sized {
    fn body_encoded(self, link: SharedLink) -> BodyEncoder<Self>;
}

impl<F> BodyEncoded for F
where
    F: Sink<Message, Error = io::Error>,
{
    fn body_encoded(self, link: SharedLink) -> BodyEncoder<Self> {
        BodyEncoder { frame: self, link }
    }
}

#[inline(always)]
fn encode(body: FrameBody) -> Message {
    const DEFAULT_FRAME_BODY_ORDERED_CHANNEL: u8 = 0;
    const DEFAULT_FRAME_BODY_PRIORITY: Priority = Priority::Medium;

    let reliability = match body {
        FrameBody::ConnectedPing { .. } => Reliability::Unreliable,
        FrameBody::ConnectedPong { .. } => Reliability::Unreliable,
        FrameBody::ConnectionRequest { .. } => Reliability::ReliableOrdered,
        FrameBody::ConnectionRequestAccepted { .. } => Reliability::Reliable,
        FrameBody::NewIncomingConnection { .. } => Reliability::ReliableOrdered,
        FrameBody::DisconnectNotification => Reliability::Reliable,
        FrameBody::DetectLostConnections => Reliability::Reliable,
        FrameBody::User(_) => {
            panic!("you should not send user packet into BodyEncoder, please send `Message`")
        }
    };
    let mut data = BytesMut::new();
    body.write(&mut data);
    Message::new(data.freeze())
        .reliability(reliability)
        .order_channel(DEFAULT_FRAME_BODY_ORDERED_CHANNEL)
        .priority(DEFAULT_FRAME_BODY_PRIORITY)
}

impl<F> BodyEncoder<F>
where
    F: Sink<Message, Error = io::Error>,
{
    /// Empty the link buffer all the frame body, insure the frame is ready to send
    pub(crate) fn poll_empty(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        if self.link.frame_body_empty() {
            return Poll::Ready(Ok(()));
        }
        let mut this = self.project();
        for body in this.link.process_frame_body() {
            ready!(this.frame.as_mut().poll_ready(cx))?;
            this.frame.as_mut().start_send(encode(body))?;
        }
        Poll::Ready(Ok(()))
    }
}

impl<F> Sink<Message> for BodyEncoder<F>
where
    F: Sink<Message, Error = io::Error>,
{
    type Error = io::Error;

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

impl<F> Sink<FrameBody> for BodyEncoder<F>
where
    F: Sink<Message, Error = io::Error>,
{
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.project().frame.poll_ready(cx))?;
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, body: FrameBody) -> Result<(), Self::Error> {
        self.project().frame.start_send(encode(body))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().poll_empty(cx))?;
        self.project().frame.poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().poll_empty(cx))?;
        self.project().frame.poll_close(cx)
    }
}
