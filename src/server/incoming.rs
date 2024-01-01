use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Bytes, BytesMut};
use flume::r#async::{RecvStream, SendSink};
use futures::{ready, FutureExt, Sink, SinkExt, Stream, StreamExt};
use pin_project_lite::pin_project;
use tracing::error;

use super::handshake::HandShaking;
use super::IO;
use crate::codec::{CodecConfig, Decoded};
use crate::errors::{CodecError, Error};
use crate::packet::{connected, Packet};
use crate::Peer;

pin_project! {
    struct Incoming<F> {
        #[pin]
        frame: F,
        router: HashMap<SocketAddr, SendSink<'static, connected::Packet<BytesMut>>>
    }
}

impl<F> Stream for Incoming<F>
where
    F: Stream<Item = (connected::Packet<BytesMut>, Peer)>
        + Sink<(Packet<Bytes>, SocketAddr), Error = CodecError>,
{
    type Item = IO;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            let Some((pack, peer)) = ready!(this.frame.poll_next_unpin(cx)) else {
                return Poll::Ready(None::<IOImpl>);
            };
            if let Some(sink) = this.router.get_mut(&peer.addr) {
                if ready!(sink.send(pack).poll_unpin(cx)).is_err() {
                    error!("connection was dropped before closed");
                    this.router.remove(&peer.addr);
                }
                continue;
            }
            let (src_tx, src_rx) = flume::unbounded();
            let (dst_tx, dst_rx) = flume::unbounded();
            this.router.insert(peer.addr, src_tx.into_sink());

            let src_stream = src_rx
                .into_stream()
                .map(Ok)
                .decoded(peer.addr, CodecConfig::default())
                .zip(futures::stream::repeat(peer))
                .handshaking();

            // TODO: add ack for src_stream

            let io = IOImpl {
                closed: false,
                dst: dst_tx.into_sink(),
                src: (),
            };
            // TODO: spawn a outgoing task here to retrieve data in dst_rx from IOImpl, and then
            // send it to frame
        }

        Poll::Pending
    }
}

struct IOImpl {
    closed: bool,
    dst: SendSink<'static, Option<Bytes>>, // None means close the connection
    src: RecvStream<'static, Bytes>,
}

impl Stream for IOImpl {
    type Item = Bytes;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.src.poll_next_unpin(cx)
    }
}

impl Sink<Bytes> for IOImpl {
    type Error = Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.closed {
            return Poll::Ready(Err(Error::ConnectionClosed("connection was closed before")));
        }
        if ready!(self.dst.poll_ready_unpin(cx)).is_err() {
            // Perhaps the connection was closed by peer, and the task exited.
            return Poll::Ready(Err(Error::ConnectionClosed("connection closed by peer")));
        }
        Poll::Ready(Ok(()))
    }

    fn start_send(mut self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        if self.closed {
            return Err(Error::ConnectionClosed("connection was closed before"));
        }
        self.dst
            .start_send_unpin(Some(item))
            .expect("must call poll_ready before start_send");
        Ok(())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if ready!(self.dst.poll_flush_unpin(cx)).is_err() {
            // Perhaps the connection was closed by peer, and the task exited.
            return Poll::Ready(Err(Error::ConnectionClosed("connection closed by peer")));
        }
        Poll::Ready(Ok(()))
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.closed {
            return Poll::Ready(Err(Error::ConnectionClosed("connection was closed before")));
        }
        self.closed = true;
        if ready!(self.dst.send(None).poll_unpin(cx)).is_err() {
            // Perhaps the connection was closed by peer, and the task exited.
            return Poll::Ready(Err(Error::ConnectionClosed("connection closed by peer")));
        }
        Poll::Ready(Ok(()))
    }
}
