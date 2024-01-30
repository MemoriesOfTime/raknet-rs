use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};

use bytes::{Bytes, BytesMut};
use flume::{Receiver, Sender};
use futures::{SinkExt, Stream};
use pin_project_lite::pin_project;
use tokio::net::UdpSocket as TokioUdpSocket;
use tokio_util::udp::UdpFramed;
use tracing::{debug, error, info};

use super::{Config, MakeIncoming};
use crate::codec::{Codec, Decoded, Encoded};
use crate::errors::CodecError;
use crate::packet::connected::{self, Frames, Reliability};
use crate::packet::{unconnected, Packet};
use crate::server::ack::{HandleIncomingAck, HandleOutgoingAck};
use crate::server::incoming::io::{AddrDropGuard, HandShakeState, IOImpl, IOState};
use crate::server::offline::HandleOffline;
use crate::server::{offline, IO};
use crate::utils::{Log, Logged};

/// Avoid stupid error: `type parameter {OfflineHandler} is part of concrete type but not used in
/// parameter list for the impl Trait type alias`
type OfflineHandler = offline::OfflineHandler<
    Log<UdpFramed<Codec, Arc<TokioUdpSocket>>, (Packet<Frames<BytesMut>>, SocketAddr), CodecError>,
>;

pin_project! {
    struct Incoming {
        #[pin]
        offline: OfflineHandler,
        config: Config,
        socket: Arc<TokioUdpSocket>,
        router: HashMap<SocketAddr, Sender<connected::Packet<Frames<BytesMut>>>>,
        drop_receiver: Receiver<SocketAddr>,
        drop_notifier: Sender<SocketAddr>,
    }
}

impl Incoming {
    fn clear_dropped_addr(self: Pin<&mut Self>) {
        let mut this = self.project();
        for addr in this.drop_receiver.try_iter() {
            this.router.remove(&addr);
            this.offline.as_mut().disconnect(&addr);
        }
    }
}

impl MakeIncoming for TokioUdpSocket {
    fn make_incoming(self, config: Config) -> impl Stream<Item = IO> {
        fn err_f(err: CodecError) {
            debug!("[frame] got codec error: {err} when decode frames");
        }

        let socket = Arc::new(self);
        let (drop_notifier, drop_receiver) = flume::unbounded();

        Incoming {
            offline: UdpFramed::new(Arc::clone(&socket), Codec)
                .logged_err(err_f)
                .handle_offline(config.offline.clone()),
            socket,
            config,
            router: HashMap::new(),
            drop_receiver,
            drop_notifier,
        }
    }
}

impl Stream for Incoming {
    type Item = IO;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.as_mut().clear_dropped_addr();

        let mut this = self.project();
        loop {
            let Some((pack, peer)) = ready!(this.offline.as_mut().poll_next(cx)) else {
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
                state: IOState::HandShaking(HandShakeState::Phase1),
                default_reliability: Reliability::ReliableOrdered,
                default_order_channel: 0,
                client_addr: AddrDropGuard {
                    client_addr: peer.addr,
                    drop_notifier: this.drop_notifier.clone(),
                },
                server_guid: this.config.offline.sever_guid,
                raw_write: UdpFramed::new(Arc::clone(this.socket), Codec).with(
                    move |u: unconnected::Packet| async move {
                        Ok::<_, CodecError>((Packet::<Frames<Bytes>>::Unconnected(u), peer.addr))
                    },
                ), // TODO implement address selector
                write: UdpFramed::new(Arc::clone(this.socket), Codec)
                    .with(move |u: Packet<Frames<Bytes>>| async move { Ok((u, peer.addr)) })
                    .handle_outgoing_ack(
                        incoming_ack_rx,
                        incoming_nack_rx,
                        outgoing_ack_rx,
                        outgoing_nack_rx,
                        this.config.send_buf_cap,
                        peer.mtu,
                    )
                    .frame_encoded(peer.mtu, this.config.codec),
                read: router_rx
                    .into_stream()
                    .handle_incoming_ack(incoming_ack_tx, incoming_nack_tx)
                    .decoded(this.config.codec, outgoing_ack_tx, outgoing_nack_tx),
            };

            return Poll::Ready(Some(io));
        }
    }
}
