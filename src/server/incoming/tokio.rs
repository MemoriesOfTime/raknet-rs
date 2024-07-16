use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};

use async_channel::Sender;
use futures::Stream;
use log::{debug, error};
use minitrace::collector::SpanContext;
use minitrace::Span;
use pin_project_lite::pin_project;
use tokio::net::UdpSocket as TokioUdpSocket;
use tokio_util::udp::UdpFramed;

use super::{Config, MakeIncoming};
use crate::codec::tokio::Codec;
use crate::codec::{Decoded, Encoded};
use crate::errors::CodecError;
use crate::guard::HandleOutgoing;
use crate::io::{SeparatedIO, IO};
use crate::link::TransferLink;
use crate::packet::connected::{self, FramesMut};
use crate::packet::Packet;
use crate::server::handler::offline;
use crate::server::handler::offline::HandleOffline;
use crate::server::handler::online::HandleOnline;
use crate::state::{IncomingStateManage, OutgoingStateManage};
use crate::utils::{Log, Logged, TraceStreamExt};

type OfflineHandler = offline::OfflineHandler<
    Log<UdpFramed<Codec, Arc<TokioUdpSocket>>, (Packet<FramesMut>, SocketAddr), CodecError>,
>;

pin_project! {
    struct Incoming {
        #[pin]
        offline: OfflineHandler,
        config: Config,
        socket: Arc<TokioUdpSocket>,
        router: HashMap<SocketAddr, Sender<connected::Packet<FramesMut>>>,
    }
}

// impl Incoming {
//     fn clear_dropped_addr(self: Pin<&mut Self>) {
//         // TODO
//     }
// }

impl MakeIncoming for TokioUdpSocket {
    fn make_incoming(self, config: Config) -> impl Stream<Item = impl IO> {
        let socket = Arc::new(self);
        Incoming {
            offline: UdpFramed::new(Arc::clone(&socket), Codec)
                .logged_err(|err| {
                    debug!("codec error: {err} when decode offline frames");
                })
                .handle_offline(config.offline_config()),
            socket,
            config,
            router: HashMap::new(),
        }
    }
}

impl Stream for Incoming {
    type Item = impl IO;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // self.as_mut().clear_dropped_addr();

        let mut this = self.project();
        loop {
            let Some((pack, peer)) = ready!(this.offline.as_mut().poll_next(cx)) else {
                return Poll::Ready(None);
            };
            if let Some(router_tx) = this.router.get_mut(&peer.addr) {
                if router_tx.try_send(pack).is_err() {
                    error!("connection was dropped before closed");
                    this.router.remove(&peer.addr);
                    this.offline.as_mut().disconnect(&peer.addr);
                }
                continue;
            }

            let (router_tx, router_rx) = async_channel::unbounded();
            router_tx.try_send(pack).unwrap();
            this.router.insert(peer.addr, router_tx);

            let link = TransferLink::new_arc(this.config.server_role());

            let dst = UdpFramed::new(Arc::clone(this.socket), Codec)
                .handle_outgoing(
                    Arc::clone(&link),
                    this.config.send_buf_cap,
                    peer.clone(),
                    this.config.server_role(),
                )
                .frame_encoded(peer.mtu, this.config.codec_config(), Arc::clone(&link))
                .manage_outgoing_state();

            let src = link
                .filter_incoming_ack(router_rx)
                .frame_decoded(
                    this.config.codec_config(),
                    Arc::clone(&link),
                    this.config.server_role(),
                )
                .manage_incoming_state()
                .handle_online(this.config.sever_guid, peer.addr, Arc::clone(&link))
                .enter_on_item(move || {
                    Span::root("conn", SpanContext::random()).with_properties(|| {
                        [
                            ("addr", peer.addr.to_string()),
                            ("mtu", peer.mtu.to_string()),
                        ]
                    })
                });

            return Poll::Ready(Some(SeparatedIO::new(src, dst)));
        }
    }
}
