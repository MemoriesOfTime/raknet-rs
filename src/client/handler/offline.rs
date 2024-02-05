use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::{Bytes, BytesMut};
use futures::{Sink, Stream};
use log::{debug, error, info, warn};
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::{self, Frames};
use crate::packet::{unconnected, Packet};
use crate::Peer;

pub(crate) struct Config {
    mtu: u16,
    client_guid: u64,
    protocol_version: u8,
}

pin_project! {
    pub(crate) struct OfflineHandler<F> {
        #[pin]
        frame: F,
        state: OfflineState,
        connected: bool,
        mtu: u16,
        server_addr: SocketAddr,
        handshaking: bool,
        config: Config,
    }
}

enum OfflineState {
    Listening,
    SendingPrepare(Option<(Packet<Frames<Bytes>>, SocketAddr)>),
    SendingFlush,
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
            match this.state {
                OfflineState::Listening => {
                    if !*this.connected && !*this.handshaking {
                        *this.state = OfflineState::SendingPrepare(Some((
                            Packet::Unconnected(unconnected::Packet::OpenConnectionRequest1 {
                                magic: (),
                                protocol_version: this.config.protocol_version,
                                mtu: this.config.mtu,
                            }),
                            *this.server_addr,
                        )));
                        *this.handshaking = true;
                        continue;
                    }
                }
                OfflineState::SendingPrepare(pack) => {
                    if let Err(err) = ready!(this.frame.as_mut().poll_ready(cx)) {
                        error!("[offline] send error: {err}");
                        *this.state = OfflineState::Listening;
                        continue;
                    }
                    if let Err(err) = this.frame.as_mut().start_send(pack.take().unwrap()) {
                        error!("[offline] send error: {err}");
                        *this.state = OfflineState::Listening;
                        continue;
                    }
                    *this.state = OfflineState::SendingFlush;
                    continue;
                }
                OfflineState::SendingFlush => {
                    if let Err(err) = ready!(this.frame.as_mut().poll_flush(cx)) {
                        error!("[offline] send error: {err}");
                    }
                    *this.state = OfflineState::Listening;
                }
            }

            let Some((packet, addr)) = ready!(this.frame.as_mut().poll_next(cx)) else {
                return Poll::Ready(None);
            };

            let pack = match packet {
                Packet::Unconnected(pack) => pack,
                Packet::Connected(pack) => {
                    if *this.connected {
                        return Poll::Ready(Some((
                            pack,
                            Peer {
                                addr: *this.server_addr,
                                mtu: *this.mtu,
                            },
                        )));
                    }
                    debug!("[offline] ignore connected packet from unconnected server");
                    *this.handshaking = false;
                    continue;
                }
            };

            let resp = match pack {
                unconnected::Packet::OpenConnectionReply1 {
                    use_encryption,
                    mtu,
                    ..
                } => {
                    if use_encryption {
                        debug!("use_encryption enabled, failed connection");
                        *this.handshaking = false;
                        continue;
                    }
                    *this.mtu = mtu;
                    unconnected::Packet::OpenConnectionRequest2 {
                        magic: (),
                        server_address: addr,
                        mtu,
                        client_guid: this.config.client_guid,
                    }
                }
                unconnected::Packet::OpenConnectionReply2 { mtu, .. } => {
                    *this.mtu = mtu;
                    *this.connected = true;
                    *this.handshaking = false;
                    debug!("connected to the server");
                    continue;
                }
                unconnected::Packet::IncompatibleProtocol {
                    server_protocol, ..
                } => {
                    if !*this.connected {
                        *this.handshaking = false;
                        info!("failed to connect to server, got incompatible protocol error, server protocol: {server_protocol}");
                    }
                    continue;
                }
                unconnected::Packet::AlreadyConnected { .. } => {
                    if !*this.connected {
                        *this.handshaking = false;
                        info!("failed to connect to server, got already connected error");
                    }
                    continue;
                }
                unconnected::Packet::ConnectionRequestFailed { .. } => {
                    info!("failed to handshake with server, got connection request failed");
                    continue;
                }
                _ => {
                    warn!(
                        "received a package({:?}) that should not be received on the client.",
                        pack.pack_type()
                    );
                    continue;
                }
            };

            *this.state = OfflineState::SendingPrepare(Some((Packet::Unconnected(resp), addr)));
        }
    }
}
