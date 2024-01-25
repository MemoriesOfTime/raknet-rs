use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::{Bytes, BytesMut};
use flume::Sender;
use futures::{ready, Sink, Stream, StreamExt};
use pin_project_lite::pin_project;
use tokio::net::UdpSocket;
use tokio_util::udp::UdpFramed;
use tracing::{debug, error};

use super::ack::HandleIncomingAck;
use super::handshake::HandShaking;
use super::offline::{self, HandleOffline};
use super::IO;
use crate::codec::{self, Codec, Decoded};
use crate::errors::{CodecError, Error};
use crate::packet::connected::Frames;
use crate::packet::{connected, Packet};
use crate::utils::{Log, Logged};

/// Instantiate here to avoid some strange generic issues.
type OfflineHandler = offline::OfflineHandler<
    Log<UdpFramed<Codec, Arc<UdpSocket>>, (Packet<Frames<BytesMut>>, SocketAddr), CodecError>,
>;

pin_project! {
    /// An async iterator that infinitely accepts connections from raknet clients, And forward UDP
    /// packets collected by the underlying layer to different connections.
    #[project(!Unpin)] // `OfflineHandler` is !Unpin, so it must be !Unpin
    struct Incoming {
        #[pin]
        offline: OfflineHandler,
        socket: Arc<UdpSocket>,
        codec_config: codec::Config,
        router: HashMap<SocketAddr, Sender<connected::Packet<Frames<BytesMut>>>>,
    }
}

impl Incoming {
    fn new(
        socket: UdpSocket,
        offline_config: offline::Config,
        codec_config: codec::Config,
    ) -> Self {
        fn err_f(err: CodecError) {
            debug!("[frame] got codec error: {err} when decode frames");
        }

        let socket = Arc::new(socket);
        Self {
            offline: UdpFramed::new(Arc::clone(&socket), Codec)
                .logged_err(err_f)
                .handle_offline(offline_config),
            socket,
            codec_config,
            router: HashMap::new(),
        }
    }

    fn disconnect(self: Pin<&mut Self>, addr: &SocketAddr) {
        let this = self.project();
        this.router.remove(addr);
        this.offline.disconnect(addr);
    }
}

impl Stream for Incoming {
    type Item = IO;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            let Some((pack, peer)) = ready!(this.offline.poll_next_unpin(cx)) else {
                return Poll::Ready(None);
            };
            if let Some(router_tx) = this.router.get_mut(&peer.addr) {
                if router_tx.send(pack).is_err() {
                    error!("connection was dropped before closed");
                    this.router.remove(&peer.addr);
                }
                continue;
            }
            let (router_tx, router_rx) = flume::unbounded();
            this.router.insert(peer.addr, router_tx);

            let (ack_tx, ack_rx) = flume::unbounded();
            let (nack_tx, nack_rx) = flume::unbounded();

            let io = IOImpl {
                // TODO: implement encoder to make it Sink<Bytes>
                output: UdpFramed::new(Arc::clone(this.socket), Codec),
                input: router_rx
                    .into_stream()
                    .handle_incoming_ack(ack_tx, nack_tx)
                    .decoded(*this.codec_config)
                    .handshaking(),
            };

            return Poll::Ready(Some(io));
        }
    }
}

pin_project! {
    /// The detailed implementation of [`IO`]connections
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
    O: Sink<(Packet<Frames<Bytes>>, SocketAddr), Error = CodecError>,
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
