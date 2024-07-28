use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::BytesMut;
use pin_project_lite::pin_project;

use crate::errors::Error;
use crate::io::Writer;
use crate::link::SharedLink;
use crate::packet::connected::FrameBody;
use crate::{Message, Reliability};

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
    F: Writer<Message>,
{
    fn body_encoded(self, link: SharedLink) -> BodyEncoder<Self> {
        BodyEncoder { frame: self, link }
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
            panic!("you should not send user packet into BodyEncoder, please send `Message`")
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

impl<F> BodyEncoder<F>
where
    F: Writer<Message>,
{
    /// Empty the link buffer all the frame body, insure the frame is ready to send
    pub(crate) fn poll_empty(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Error>> {
        let mut this = self.project();
        if this.link.frame_body_empty() {
            return Poll::Ready(Ok(()));
        }

        ready!(this.frame.as_mut().poll_ready(cx))?;

        // frame is now ready to send
        for body in this.link.process_frame_body() {
            this.frame.as_mut().feed(encode(body));
            // ready for next frame
            ready!(this.frame.as_mut().poll_ready(cx))?;
        }
        Poll::Ready(Ok(()))
    }
}

impl<F> Writer<Message> for BodyEncoder<F>
where
    F: Writer<Message>,
{
    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Writer::<FrameBody>::poll_ready(self, cx)
    }

    fn feed(self: Pin<&mut Self>, item: Message) {
        // skip encode
        self.project().frame.feed(item);
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Writer::<FrameBody>::poll_flush(self, cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Writer::<FrameBody>::poll_close(self, cx)
    }
}

impl<F> Writer<FrameBody> for BodyEncoder<F>
where
    F: Writer<Message>,
{
    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        ready!(self.as_mut().poll_empty(cx))?;
        // there is literally no buffer
        debug_assert!(self.link.frame_body_empty());
        Poll::Ready(Ok(()))
    }

    fn feed(self: Pin<&mut Self>, body: FrameBody) {
        self.project().frame.feed(encode(body));
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        ready!(self.as_mut().poll_empty(cx))?;
        debug_assert!(self.link.frame_body_empty());
        self.project().frame.poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        ready!(self.as_mut().poll_empty(cx))?;
        debug_assert!(self.link.frame_body_empty());
        self.project().frame.poll_close(cx)
    }
}
