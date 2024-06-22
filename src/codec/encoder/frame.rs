use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::BytesMut;
use futures::Sink;
use minitrace::local::LocalSpan;
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::{FrameBody, Reliability};
use crate::Message;

pin_project! {
    pub(crate) struct FrameEncoder<F> {
        #[pin]
        frame: F
    }
}

pub(crate) trait FrameEncoded: Sized {
    fn frame_encoded(self) -> FrameEncoder<Self>;
}

impl<F> FrameEncoded for F
where
    F: Sink<Message, Error = CodecError>,
{
    fn frame_encoded(self) -> FrameEncoder<Self> {
        FrameEncoder { frame: self }
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

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, body: FrameBody) -> Result<(), Self::Error> {
        // Reliabilities with some build-in packet
        let _span = LocalSpan::enter_with_local_parent("codec.frame")
            .with_property(|| ("frame_body_type", format!("{body:?}")));

        let reliability = match body {
            FrameBody::ConnectedPing { .. } => Reliability::Unreliable,
            FrameBody::ConnectedPong { .. } => Reliability::Unreliable,
            FrameBody::ConnectionRequest { .. } => Reliability::ReliableOrdered,
            FrameBody::ConnectionRequestAccepted { .. } => Reliability::Reliable,
            FrameBody::NewIncomingConnection { .. } => Reliability::ReliableOrdered,
            FrameBody::DisconnectNotification => {
                panic!("you should not send DisconnectNotification, use .poll_close(cx) instead")
            }
            FrameBody::DetectLostConnections => Reliability::Reliable,
            FrameBody::User(_) => {
                panic!("you should not send user packet into FrameEncoder, please send `Message`")
            }
        };

        let mut data = BytesMut::new();
        body.write(&mut data);

        self.project()
            .frame
            .start_send(Message::new(reliability, 0, data.freeze()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_close(cx)
    }
}
