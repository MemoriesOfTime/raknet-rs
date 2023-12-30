use std::collections::HashSet;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::{ready, FutureExt, Sink, SinkExt, Stream};
use pin_project_lite::pin_project;
use tracing::{debug, error, warn};

use crate::errors::CodecError;
use crate::packet::{connected, unconnected, PackId, Packet};

struct Config {
    sever_guid: u64,
    advertisement: Bytes,
    min_mtu: u16,
    max_mtu: u16,
    // Supported raknet versions, sorted
    support_version: Vec<u8>,
}

pin_project! {
    /// OfflineHandler takes a Packet frames stream and convert to connected::Packet stream
    struct OfflineHandler<F> {
        #[pin]
        frame: F,
        config: Config,
        pending: lru::LruCache<SocketAddr, u8>,
        connected: HashSet<SocketAddr>, // TODO: Support a client opening multiple RakNet connections, that is, using the client GUID.
    }
}

impl<F> OfflineHandler<F>
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
}

impl<F> Stream for OfflineHandler<F>
where
    F: Stream<Item = (Packet<Bytes>, SocketAddr)>
        + Sink<(Packet<Bytes>, SocketAddr), Error = CodecError>,
{
    type Item = (connected::Packet<Bytes>, SocketAddr);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            let Some((packet, addr)) = ready!(this.frame.as_mut().poll_next(cx)) else {
                return Poll::Ready(None);
            };
            let pack = match packet {
                Packet::Unconnected(pack) => pack,
                Packet::Connected(pack) => return Poll::Ready(Some((pack, addr))),
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
                    }
                    // client should adjust the mtu
                    if mtu < this.config.min_mtu
                        || mtu > this.config.max_mtu
                        || !this.connected.insert(addr)
                    {
                        let mut send = this
                            .frame
                            .send((Self::make_already_connected(this.config), addr));
                        if let Err(err) = ready!(send.poll_unpin(cx)) {
                            error!("failed send already connected to {addr}, error {err}");
                        }
                        continue;
                    }
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
                        pack.pack_id()
                    );
                    continue;
                }
            };
        }
    }
}

impl<F> Sink<(Packet<Bytes>, SocketAddr)> for OfflineHandler<F>
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
            if matches!(frame_set.inner_pack_id()?, PackId::DisconnectNotification) {
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
