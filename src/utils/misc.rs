use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::Sink;
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::Frames;
use crate::packet::{unconnected, Packet};

/// Sink with a provided address
pub(crate) trait WithAddress: Sized {
    fn with_addr(self, addr: SocketAddr) -> WithAddr<Self>;
}

impl<F> WithAddress for F
where
    F: Sink<(Packet<Frames<Bytes>>, SocketAddr), Error = CodecError>,
{
    fn with_addr(self, addr: SocketAddr) -> WithAddr<Self> {
        WithAddr { addr, frame: self }
    }
}

pin_project! {
    pub(crate) struct WithAddr<F> {
        addr: SocketAddr,
        #[pin]
        frame: F,
    }
}

impl<F> Sink<unconnected::Packet> for WithAddr<F>
where
    F: Sink<(Packet<Frames<Bytes>>, SocketAddr), Error = CodecError>,
{
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: unconnected::Packet) -> Result<(), Self::Error> {
        let addr = self.addr;
        self.project()
            .frame
            .start_send((Packet::Unconnected(item), addr))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_close(cx)
    }
}

impl<F> Sink<Packet<Frames<Bytes>>> for WithAddr<F>
where
    F: Sink<(Packet<Frames<Bytes>>, SocketAddr), Error = CodecError>,
{
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Packet<Frames<Bytes>>) -> Result<(), Self::Error> {
        let addr = self.addr;
        self.project().frame.start_send((item, addr))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_close(cx)
    }
}
