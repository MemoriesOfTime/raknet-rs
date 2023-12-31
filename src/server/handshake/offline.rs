use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::{ready, FutureExt, Sink, SinkExt, Stream};
use pin_project_lite::pin_project;
use tracing::{debug, error, warn};

use crate::errors::CodecError;
use crate::packet::{connected, unconnected, PackType, Packet};

#[derive(Debug, Clone)]
pub(super) struct Config {
    sever_guid: u64,
    advertisement: Bytes,
    min_mtu: u16,
    max_mtu: u16,
    // Supported raknet versions, sorted
    support_version: Vec<u8>,
}

#[derive(Debug, Clone)]
pub(super) struct Peer {
    pub(super) addr: SocketAddr,
    pub(super) mtu: u16,
}

pin_project! {
    /// OfflineHandShake takes the codec frame and perform offline handshake.
    struct OfflineHandShake<F> {
        #[pin]
        frame: F,
        config: Config,
        pending: lru::LruCache<SocketAddr, u8>,
        connected: HashMap<SocketAddr, Peer>,
    }
}

impl<F> OfflineHandShake<F>
where
    F: Sink<(Packet<Bytes>, SocketAddr), Error = CodecError>,
{
    fn make_incompatible_version(config: &Config) -> Packet<Bytes> {
        Packet::Unconnected(unconnected::Packet::IncompatibleProtocol {
            server_protocol: *config.support_version.last().unwrap(),
            magic: (),
            server_guid: config.sever_guid,
        })
    }

    fn make_already_connected(config: &Config) -> Packet<Bytes> {
        Packet::Unconnected(unconnected::Packet::AlreadyConnected {
            magic: (),
            server_guid: config.sever_guid,
        })
    }

    fn make_connection_request_failed(config: &Config) -> Packet<Bytes> {
        Packet::Unconnected(unconnected::Packet::ConnectionRequestFailed {
            magic: (),
            server_guid: config.sever_guid,
        })
    }
}

impl<F> Stream for OfflineHandShake<F>
where
    F: Stream<Item = (Packet<Bytes>, SocketAddr)>
        + Sink<(Packet<Bytes>, SocketAddr), Error = CodecError>,
{
    type Item = (connected::Packet<Bytes>, Peer);

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
                unconnected::Packet::OpenConnectionRequest2 {
                    mtu, client_guid, ..
                } => {
                    let Some(proto_ver) = this.pending.pop(&addr) else {
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

impl<F> Sink<(Packet<Bytes>, SocketAddr)> for OfflineHandShake<F>
where
    F: Sink<(Packet<Bytes>, SocketAddr), Error = CodecError>,
{
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_ready(cx)
    }

    fn start_send(
        self: Pin<&mut Self>,
        (packet, addr): (Packet<Bytes>, SocketAddr),
    ) -> Result<(), Self::Error> {
        let this = self.project();
        if let Packet::Connected(connected::Packet::FrameSet(frame_set)) = &packet {
            if frame_set.first_pack_type() == PackType::DisconnectNotification {
                debug!("disconnect from {}, clean it's frame parts buffer", addr);
                this.connected.remove(&addr);
                this.pending.pop(&addr);
            }
        };
        this.frame.start_send((packet, addr))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_close(cx)
    }
}
