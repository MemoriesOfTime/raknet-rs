use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::{Sink, Stream};
use pin_project_lite::pin_project;

use crate::errors::Error;

pub(crate) enum Conn<F> {
    HandShaking(HandeShake<F>),
    Connected(Fragment<F>),
    Closed,
}

pin_project! {
    /// Process a raknet handshake, poll it to open a connection to the client
    pub(crate) struct HandeShake<F> {
        #[pin]
        pub(crate) frame: F,
        pub(crate) client_protocol_ver: u8,
        pub(crate) client_mtu: u16,
        pub(crate) peer_addr: SocketAddr,
    }
}

pin_project! {
    /// Fragment/Defragment the packet sink/stream (UdpFramed). Enable external production and
    /// consumption of continuous packets.
    pub(crate) struct Fragment<F> {
        #[pin]
        frame: F,
        peer_addr: SocketAddr,
    }
}

impl<F> Future for HandeShake<F> {
    // TODO maybe handshake error here would be better
    type Output = Result<F, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        todo!()
    }
}

// TODO to be determined
impl<F> Stream for Fragment<F> {
    type Item = ();

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        todo!()
    }
}

// TODO to be determined
impl<F> Sink<()> for Fragment<F> {
    type Error = ();

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }

    fn start_send(self: Pin<&mut Self>, item: ()) -> Result<(), Self::Error> {
        todo!()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }
}
