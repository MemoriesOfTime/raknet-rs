use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::{Bytes, BytesMut};
use flume::r#async::SendSink;
use futures::{ready, FutureExt, Sink, SinkExt, Stream, StreamExt};
use pin_project_lite::pin_project;
use tokio::net::UdpSocket;
use tracing::error;

use super::ack::Acknowledge;
use super::handshake::HandShaking;
use super::offline::{self, HandleOffline};
use super::IO;
use crate::codec::{self, Decoded, Framed};
use crate::errors::{CodecError, Error};
use crate::packet::{connected, Packet};

struct Config {
    offline: offline::Config,
    codec: codec::Config,
}

struct Incoming {
    socket: Arc<UdpSocket>,
    config: Config,
    router: HashMap<SocketAddr, SendSink<'static, connected::Packet<BytesMut>>>,
}

impl Stream for Incoming {
    type Item = IO;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut frame = Arc::clone(&self.socket)
            .framed()
            .handle_offline(self.config.offline.clone());
        loop {
            let Some((pack, peer)) = ready!(frame.poll_next_unpin(cx)) else {
                return Poll::Ready(None);
            };
            if let Some(router_tx) = self.router.get_mut(&peer.addr) {
                if ready!(router_tx.send(pack).poll_unpin(cx)).is_err() {
                    error!("connection was dropped before closed");
                    self.router.remove(&peer.addr);
                }
                continue;
            }
            let (router_tx, router_rx) = flume::unbounded();
            self.router.insert(peer.addr, router_tx.into_sink());

            let io = IOImpl {
                output: frame,
                input: router_rx
                    .into_stream()
                    .decoded(self.config.codec)
                    .handshaking()
                    .ack(),
            };

            return Poll::Ready(Some(io));
        }
    }
}

pin_project! {
    struct IOImpl<I, O> {
        #[pin]
        input: I,
        #[pin]
        output: O,
    }
}

impl<I, O> Stream for IOImpl<I, O>
where
    I: Stream<Item = Bytes>,
{
    type Item = Bytes;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().input.poll_next_unpin(cx)
    }
}

impl<I, O> Sink<Bytes> for IOImpl<I, O>
where
    O: Sink<(Packet<Bytes>, SocketAddr), Error = CodecError>,
{
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }

    fn start_send(self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        todo!()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }
}
