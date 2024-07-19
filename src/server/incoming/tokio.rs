use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};

use async_channel::Sender;
use concurrent_queue::ConcurrentQueue;
use futures::Stream;
use log::{debug, error};
use minitrace::collector::SpanContext;
use minitrace::Span;
use pin_project_lite::pin_project;
use tokio::net::UdpSocket as TokioUdpSocket;

use super::{Config, MakeIncoming};
use crate::codec::frame::Framed;
use crate::codec::{Decoded, Encoded};
use crate::guard::HandleOutgoing;
use crate::io::{SeparatedIO, IO};
use crate::link::{SharedLink, TransferLink};
use crate::packet::connected::{self, FrameSet, FramesMut};
use crate::server::handler::offline::OfflineHandler;
use crate::server::handler::online::HandleOnline;
use crate::state::{CloseOnDrop, IncomingStateManage, OutgoingStateManage};
use crate::utils::TraceStreamExt;

struct RouteEntry {
    router_tx: Sender<FrameSet<FramesMut>>,
    link: SharedLink,
}

impl RouteEntry {
    fn new(link: SharedLink) -> (Self, impl Stream<Item = FrameSet<FramesMut>>) {
        let (router_tx, router_rx) = async_channel::unbounded();
        (Self { router_tx, link }, router_rx)
    }

    /// Deliver the packet to the corresponding router. Return false if the connection was dropped.
    #[inline]
    fn deliver(&self, pack: connected::Packet<FramesMut>) -> bool {
        if self.router_tx.is_closed() {
            debug_assert!(Arc::strong_count(&self.link) == 1);
            return false;
        }
        match pack {
            connected::Packet::FrameSet(frames) => self.router_tx.try_send(frames).unwrap(),
            connected::Packet::Ack(ack) => self.link.incoming_ack(ack),
            connected::Packet::Nack(nack) => self.link.incoming_nack(nack),
        };
        true
    }
}

pin_project! {
    struct Incoming {
        #[pin]
        offline: OfflineHandler<Framed<Arc<TokioUdpSocket>>>,
        config: Config,
        socket: Arc<TokioUdpSocket>,
        router: HashMap<SocketAddr, RouteEntry>,
        close_events: Arc<ConcurrentQueue<SocketAddr>>,
    }
}

impl MakeIncoming for TokioUdpSocket {
    fn make_incoming(self, config: Config) -> impl Stream<Item = impl IO> {
        let socket = Arc::new(self);
        Incoming {
            offline: OfflineHandler::new(
                Framed::new(Arc::clone(&socket), config.max_mtu as usize),
                config.offline_config(),
            ),
            socket,
            config,
            router: HashMap::new(),
            close_events: Arc::new(ConcurrentQueue::unbounded()),
        }
    }
}

impl Stream for Incoming {
    type Item = impl IO;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        let role = this.config.server_role();
        for ev in this.close_events.try_iter() {
            this.router
                .remove(&ev)
                .expect("closed a non-exist connection");
            this.offline.as_mut().disconnect(&ev);
            debug!("[{role}] connection closed: {ev}");
        }

        loop {
            let Some((pack, peer)) = ready!(this.offline.as_mut().poll_next(cx)) else {
                return Poll::Ready(None);
            };
            if let Some(entry) = this.router.get_mut(&peer.addr) {
                if !entry.deliver(pack) {
                    error!("[{role}] connection was dropped before closed");
                    this.router.remove(&peer.addr);
                    this.offline.as_mut().disconnect(&peer.addr);
                }
                continue;
            }

            let link = TransferLink::new_arc(role);
            let (entry, route) = RouteEntry::new(Arc::clone(&link));
            entry.deliver(pack);
            this.router.insert(peer.addr, entry);

            let dst = Framed::new(Arc::clone(this.socket), this.config.max_mtu as usize)
                .handle_outgoing(
                    Arc::clone(&link),
                    this.config.send_buf_cap,
                    peer.clone(),
                    role,
                )
                .frame_encoded(peer.mtu, this.config.codec_config(), Arc::clone(&link))
                .manage_outgoing_state(Some(CloseOnDrop::new(
                    peer.addr,
                    Arc::clone(this.close_events),
                )));

            let src = route
                .frame_decoded(this.config.codec_config(), Arc::clone(&link), role)
                .manage_incoming_state()
                .handle_online(role, peer.addr, Arc::clone(&link))
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
