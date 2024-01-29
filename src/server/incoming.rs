use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::{Bytes, BytesMut};
use flume::Sender;
use futures::{ready, Sink, SinkExt, Stream, StreamExt};
use pin_project_lite::pin_project;
use tokio::net::UdpSocket;
use tokio_util::udp::UdpFramed;
use tracing::{debug, error, info};

use super::ack::{HandleIncomingAck, HandleOutgoingAck};
use super::handshake::HandShaking;
use super::offline::{self, HandleOffline};
use super::{IOpts, Message, IO};
use crate::codec::{self, Codec, Decoded, Encoded};
use crate::errors::{CodecError, Error};
use crate::packet::connected::{Frames, Reliability};
use crate::packet::{connected, Packet};
use crate::utils::{Log, Logged};

#[derive(Debug, Clone, derive_builder::Builder)]
pub struct Config {
    send_buf_cap: usize,
    codec: codec::Config,
    offline: offline::Config,
}

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
        config: Config,
        router: HashMap<SocketAddr, Sender<connected::Packet<Frames<BytesMut>>>>,
    }
}

impl Incoming {
    fn new(socket: UdpSocket, config: Config) -> Self {
        fn err_f(err: CodecError) {
            debug!("[frame] got codec error: {err} when decode frames");
        }

        let socket = Arc::new(socket);
        Self {
            offline: UdpFramed::new(Arc::clone(&socket), Codec)
                .logged_err(err_f)
                .handle_offline(config.offline.clone()),
            socket,
            config,
            router: HashMap::new(),
        }
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
                    this.offline.as_mut().disconnect(&peer.addr);
                }
                continue;
            }
            info!("new incoming from {}", peer.addr);

            let (router_tx, router_rx) = flume::unbounded();
            this.router.insert(peer.addr, router_tx);

            let (incoming_ack_tx, incoming_ack_rx) = flume::unbounded();
            let (incoming_nack_tx, incoming_nack_rx) = flume::unbounded();

            let (outgoing_ack_tx, outgoing_ack_rx) = flume::unbounded();
            let (outgoing_nack_tx, outgoing_nack_rx) = flume::unbounded();

            let io = IOImpl {
                output: UdpFramed::new(Arc::clone(this.socket), Codec)
                    .with(move |u: Packet<Frames<Bytes>>| async move { Ok((u, peer.addr)) })
                    .handle_outgoing_ack(
                        incoming_ack_rx,
                        incoming_nack_rx,
                        outgoing_ack_rx,
                        outgoing_nack_rx,
                        this.config.send_buf_cap,
                        peer.mtu,
                    )
                    .encoded(peer.mtu, this.config.codec),
                input: router_rx
                    .into_stream()
                    .handle_incoming_ack(incoming_ack_tx, incoming_nack_tx)
                    .decoded(this.config.codec, outgoing_ack_tx, outgoing_nack_tx)
                    .handshaking(),
                default_reliability: Reliability::ReliableOrdered,
                default_order_channel: 0,
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
        default_reliability: Reliability,
        default_order_channel: u8,
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

impl<I, O> IOpts for IOImpl<I, O> {
    fn set_default_reliability(&mut self, reliability: Reliability) {
        self.default_reliability = reliability;
    }

    fn get_default_reliability(&self) -> Reliability {
        self.default_reliability
    }

    fn set_default_order_channel(&mut self, order_channel: u8) {
        self.default_order_channel = order_channel;
    }

    fn get_default_order_channel(&self) -> u8 {
        self.default_order_channel
    }
}

impl<I, O> Sink<Bytes> for IOImpl<I, O>
where
    O: Sink<Message, Error = CodecError>,
{
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<Message>::poll_ready(self, cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        let msg = Message::new(self.default_reliability, self.default_order_channel, item);
        Sink::<Message>::start_send(self, msg)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<Message>::poll_flush(self, cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<Message>::poll_close(self, cx)
    }
}

impl<I, O> Sink<Message> for IOImpl<I, O>
where
    O: Sink<Message, Error = CodecError>,
{
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.project().output.poll_ready(cx))?;
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        self.project().output.start_send(item)?;
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.project().output.poll_flush(cx))?;
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.project().output.poll_close(cx))?;
        Poll::Ready(Ok(()))
    }
}
