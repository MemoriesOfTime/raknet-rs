use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};

use concurrent_queue::ConcurrentQueue;
use fastrace::collector::SpanContext;
use fastrace::Span;
use futures::Stream;
use log::{debug, error};
use pin_project_lite::pin_project;
use tokio::net::UdpSocket as TokioUdpSocket;

use super::{Config, MakeIncoming};
use crate::codec::frame::Framed;
use crate::codec::{Decoded, Encoded};
use crate::guard::HandleOutgoing;
use crate::io::{SeparatedIO, IO};
use crate::link::{Router, TransferLink};
use crate::server::handler::offline::OfflineHandler;
use crate::server::handler::online::HandleOnline;
use crate::state::{CloseOnDrop, IncomingStateManage, OutgoingStateManage};
use crate::utils::TraceStreamExt;

pin_project! {
    struct Incoming {
        #[pin]
        offline: OfflineHandler<Framed<Arc<TokioUdpSocket>>>,
        config: Config,
        socket: Arc<TokioUdpSocket>,
        routers: HashMap<SocketAddr, Router>,
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
            routers: HashMap::new(),
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
            this.routers
                .remove(&ev)
                .expect("closed a non-exist connection");
            this.offline.as_mut().disconnect(&ev);
            debug!("[{role}] connection closed: {ev}");
        }

        loop {
            let Some((pack, peer)) = ready!(this.offline.as_mut().poll_next(cx)) else {
                return Poll::Ready(None);
            };
            if let Some(entry) = this.routers.get_mut(&peer.addr) {
                if !entry.deliver(pack) {
                    error!("[{role}] connection was dropped before closed");
                }
                continue;
            }

            let link = TransferLink::new_arc(role, peer);
            let (mut entry, route) = Router::new(Arc::clone(&link));
            entry.deliver(pack);
            this.routers.insert(peer.addr, entry);

            let dst = Framed::new(Arc::clone(this.socket), this.config.max_mtu as usize)
                .handle_outgoing(Arc::clone(&link), this.config.send_buf_cap, peer, role)
                .frame_encoded(peer.mtu, this.config.codec_config(), Arc::clone(&link))
                .manage_outgoing_state(Some(CloseOnDrop::new(
                    peer.addr,
                    Arc::clone(this.close_events),
                )));

            let src = route
                .frame_decoded(this.config.codec_config(), role)
                .manage_incoming_state()
                .handle_online(role, peer.addr, Arc::clone(&link))
                .enter_on_item(move || {
                    Span::root("online", SpanContext::random()).with_properties(|| {
                        [
                            ("peer_guid", peer.guid.to_string()),
                            ("peer_addr", peer.addr.to_string()),
                            ("conn_mtu", peer.mtu.to_string()),
                        ]
                    })
                });

            return Poll::Ready(Some(SeparatedIO::new(src, dst)));
        }
    }
}
