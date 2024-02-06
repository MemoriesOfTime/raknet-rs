use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::{Bytes, BytesMut};
use futures::{Sink, Stream};
use log::debug;
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::{self, Frames};
use crate::packet::{unconnected, Packet};

#[derive(Debug, Clone, Copy, derive_builder::Builder)]
pub struct Config {
    pub(crate) mtu: u16,
    pub(crate) client_guid: u64,
    protocol_version: u8,
}

pub(crate) trait HandleOffline: Sized {
    fn handle_offline(self, server_addr: SocketAddr, config: Config) -> OfflineHandler<Self>;
}

impl<F> HandleOffline for F
where
    F: Stream<Item = (Packet<Frames<BytesMut>>, SocketAddr)>
        + Sink<(Packet<Frames<Bytes>>, SocketAddr), Error = CodecError>,
{
    fn handle_offline(self, server_addr: SocketAddr, config: Config) -> OfflineHandler<Self> {
        OfflineHandler {
            frame: self,
            state: State::SendOpenConnectionRequest1(Packet::Unconnected(
                unconnected::Packet::OpenConnectionRequest1 {
                    magic: (),
                    protocol_version: config.protocol_version,
                    mtu: config.mtu,
                },
            )),
            server_addr,
            config,
        }
    }
}

pin_project! {
    pub(crate) struct OfflineHandler<F> {
        #[pin]
        frame: F,
        state: State,
        server_addr: SocketAddr,
        config: Config,
    }
}

enum State {
    SendOpenConnectionRequest1(Packet<Frames<Bytes>>),
    WaitOpenConnectionReply1,
    SendOpenConnectionRequest2(Packet<Frames<Bytes>>),
    WaitOpenConnectionReply2,
    Connected,
}

impl<F> Stream for OfflineHandler<F>
where
    F: Stream<Item = (Packet<Frames<BytesMut>>, SocketAddr)>
        + Sink<(Packet<Frames<Bytes>>, SocketAddr), Error = CodecError>,
{
    type Item = connected::Packet<Frames<BytesMut>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            match this.state {
                State::SendOpenConnectionRequest1(pack) => {
                    if let Err(err) = ready!(this.frame.as_mut().poll_ready(cx)) {
                        debug!(
                            "[offline] SendingOpenConnectionRequest1 poll_ready error: {err}, retrying"
                        );
                        continue;
                    }
                    if let Err(err) = this
                        .frame
                        .as_mut()
                        .start_send((pack.clone(), *this.server_addr))
                    {
                        debug!(
                            "[offline] SendingOpenConnectionRequest1 start_send error: {err}, retrying"
                        );
                        continue;
                    }
                    if let Err(err) = ready!(this.frame.as_mut().poll_flush(cx)) {
                        debug!(
                            "[offline] SendingOpenConnectionRequest1 poll_flush error: {err}, retrying"
                        );
                        continue;
                    }
                    *this.state = State::WaitOpenConnectionReply1;
                }
                State::WaitOpenConnectionReply1 => {
                    let Some((pack, addr)) = ready!(this.frame.as_mut().poll_next(cx)) else {
                        return Poll::Ready(None);
                    };
                    if addr != *this.server_addr {
                        continue;
                    }
                    let next = match pack {
                        Packet::Unconnected(unconnected::Packet::OpenConnectionReply1 {
                            mtu,
                            ..
                        }) => Packet::Unconnected(unconnected::Packet::OpenConnectionRequest2 {
                            magic: (),
                            server_address: *this.server_addr,
                            mtu,
                            client_guid: this.config.client_guid,
                        }),
                        _ => continue,
                    };
                    *this.state = State::SendOpenConnectionRequest2(next);
                }
                State::SendOpenConnectionRequest2(pack) => {
                    if let Err(err) = ready!(this.frame.as_mut().poll_ready(cx)) {
                        debug!(
                            "[offline] SendOpenConnectionRequest2 poll_ready error: {err}, retrying"
                        );
                        continue;
                    }
                    if let Err(err) = this
                        .frame
                        .as_mut()
                        .start_send((pack.clone(), *this.server_addr))
                    {
                        debug!(
                            "[offline] SendOpenConnectionRequest2 start_send error: {err}, retrying"
                        );
                        continue;
                    }
                    if let Err(err) = ready!(this.frame.as_mut().poll_flush(cx)) {
                        debug!(
                            "[offline] SendOpenConnectionRequest2 poll_flush error: {err}, retrying"
                        );
                        continue;
                    }
                    *this.state = State::WaitOpenConnectionReply2;
                }
                State::WaitOpenConnectionReply2 => {
                    let Some((pack, addr)) = ready!(this.frame.as_mut().poll_next(cx)) else {
                        return Poll::Ready(None);
                    };
                    if addr != *this.server_addr {
                        continue;
                    }
                    match pack {
                        Packet::Unconnected(unconnected::Packet::OpenConnectionReply2 {
                            ..
                        }) => {}
                        _ => continue,
                    };
                    *this.state = State::Connected;
                }
                State::Connected => {
                    let Some((pack, addr)) = ready!(this.frame.as_mut().poll_next(cx)) else {
                        return Poll::Ready(None);
                    };
                    if addr != *this.server_addr {
                        continue;
                    }
                    match pack {
                        Packet::Connected(pack) => return Poll::Ready(Some(pack)),
                        _ => continue,
                    };
                }
            }
        }
    }
}
