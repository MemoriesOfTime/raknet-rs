use std::collections::HashMap;
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Bytes, BytesMut};
use futures::{ready, FutureExt, Sink, SinkExt, Stream};
use pin_project_lite::pin_project;
use tracing::{debug, error, warn};

use crate::errors::CodecError;
use crate::packet::connected::Frames;
use crate::packet::{connected, unconnected, Packet};
use crate::Peer;

pub(super) trait HandleOffline: Sized {
    fn handle_offline(self, config: Config) -> OfflineHandler<Self>;
}

impl<F> HandleOffline for F {
    fn handle_offline(self, config: Config) -> OfflineHandler<Self> {
        OfflineHandler {
            frame: self,
            pending: lru::LruCache::new(
                NonZeroUsize::new(config.max_pending).expect("max_pending > 0"),
            ),
            config,
            connected: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Config {
    sever_guid: u64,
    advertisement: Bytes,
    min_mtu: u16,
    max_mtu: u16,
    // Supported raknet versions, sorted
    support_version: Vec<u8>,
    max_pending: usize,
}

pin_project! {
    /// OfflineHandler takes the codec frame and perform offline handshake.
    pub(super) struct OfflineHandler<F> {
        #[pin]
        frame: F,
        config: Config,
        pending: lru::LruCache<SocketAddr, u8>,
        connected: HashMap<SocketAddr, Peer>,
    }
}

impl<F> OfflineHandler<F> {
    pub(super) fn disconnect(&mut self, addr: &SocketAddr) {
        self.pending.pop(addr);
        self.connected.remove(addr);
    }
}

impl<F> OfflineHandler<F>
where
    F: Sink<(Packet<Frames<Bytes>>, SocketAddr), Error = CodecError>,
{
    fn make_incompatible_version(config: &Config) -> Packet<Frames<Bytes>> {
        Packet::Unconnected(unconnected::Packet::IncompatibleProtocol {
            server_protocol: *config.support_version.last().unwrap(),
            magic: (),
            server_guid: config.sever_guid,
        })
    }

    fn make_already_connected(config: &Config) -> Packet<Frames<Bytes>> {
        Packet::Unconnected(unconnected::Packet::AlreadyConnected {
            magic: (),
            server_guid: config.sever_guid,
        })
    }

    fn make_connection_request_failed(config: &Config) -> Packet<Frames<Bytes>> {
        Packet::Unconnected(unconnected::Packet::ConnectionRequestFailed {
            magic: (),
            server_guid: config.sever_guid,
        })
    }
}

impl<F> Stream for OfflineHandler<F>
where
    F: Stream<Item = (Packet<Frames<BytesMut>>, SocketAddr)>
        + Sink<(Packet<Frames<Bytes>>, SocketAddr), Error = CodecError>,
{
    type Item = (connected::Packet<Frames<BytesMut>>, Peer);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            let Some((packet, addr)) = ready!(this.frame.as_mut().poll_next(cx)) else {
                return Poll::Ready(None);
            };
            let pack = match packet {
                Packet::Unconnected(pack) => pack,
                Packet::Connected(pack) => {
                    if let Some(peer) = this.connected.get(&addr) {
                        return Poll::Ready(Some((pack, peer.clone())));
                    }
                    debug!("ignore connected packet from unconnected client {addr}");
                    // TODO: Send DETECT_LOST_CONNECTION ?
                    let mut send = this
                        .frame
                        .send((Self::make_connection_request_failed(this.config), addr));
                    if let Err(err) = ready!(send.poll_unpin(cx)) {
                        error!("failed send connection request failed to {addr}, error {err}");
                    }
                    continue;
                }
            };
            match pack {
                unconnected::Packet::UnconnectedPing { send_timestamp, .. } => {
                    unconnected::Packet::UnconnectedPong {
                        send_timestamp,
                        server_guid: this.config.sever_guid,
                        magic: (),
                        data: this.config.advertisement.clone(),
                    }
                }
                unconnected::Packet::OpenConnectionRequest1 {
                    protocol_version,
                    mtu,
                    ..
                } => {
                    if this
                        .config
                        .support_version
                        .binary_search(&protocol_version)
                        .is_err()
                    {
                        let mut send = this
                            .frame
                            .send((Self::make_incompatible_version(this.config), addr));
                        if let Err(err) = ready!(send.poll_unpin(cx)) {
                            error!("failed send incompatible version to {addr}, error {err}");
                        }
                        continue;
                    }
                    if this.pending.put(addr, protocol_version).is_some() {
                        debug!("received duplicate open connection request 1 from {addr}");
                    }
                    // max_mtu >= final_mtu >= min_mtu
                    let final_mtu = this.config.max_mtu.min(this.config.min_mtu.max(mtu));
                    unconnected::Packet::OpenConnectionReply1 {
                        magic: (),
                        server_guid: this.config.sever_guid,
                        use_encryption: false, // must set to false first
                        mtu: final_mtu,
                    }
                }
                unconnected::Packet::OpenConnectionRequest2 { mtu, .. } => {
                    if this.pending.pop(&addr).is_none() {
                        debug!("received open connection request 2 from {addr} without open connection request 1");
                        let mut send = this
                            .frame
                            .send((Self::make_incompatible_version(this.config), addr));
                        if let Err(err) = ready!(send.poll_unpin(cx)) {
                            error!("failed send incompatible version to {addr}, error {err}");
                        }
                        continue;
                    };
                    // client should adjust the mtu
                    if mtu < this.config.min_mtu
                        || mtu > this.config.max_mtu
                        || this.connected.contains_key(&addr)
                    {
                        let mut send = this
                            .frame
                            .send((Self::make_already_connected(this.config), addr));
                        if let Err(err) = ready!(send.poll_unpin(cx)) {
                            error!("failed send already connected to {addr}, error {err}");
                        }
                        continue;
                    }
                    this.connected.insert(addr, Peer { addr, mtu });
                    unconnected::Packet::OpenConnectionReply2 {
                        magic: (),
                        server_guid: this.config.sever_guid,
                        client_address: addr,
                        mtu,
                        encryption_enabled: false, // must set to false
                    }
                }
                _ => {
                    warn!(
                        "received a package({:?}) that should not be received on the server.",
                        pack.pack_type()
                    );
                    continue;
                }
            };
        }
    }
}
