use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use futures::{Future, Sink, SinkExt, Stream, StreamExt};
use log::debug;
use pin_project_lite::pin_project;

use crate::errors::{CodecError, Error};
use crate::packet::connected::{self, Frames, FramesMut};
use crate::packet::{unconnected, Packet};

#[derive(Debug, Clone, Copy, derive_builder::Builder)]
pub(crate) struct Config {
    mtu: u16,
    client_guid: u64,
    protocol_version: u8,
}

pub(crate) trait HandleOffline: Sized {
    fn handle_offline(self, server_addr: SocketAddr, config: Config) -> OfflineHandler<Self>;
}

impl<F> HandleOffline for F
where
    F: Stream<Item = (Packet<FramesMut>, SocketAddr)>
        + Sink<(Packet<Frames>, SocketAddr), Error = CodecError>,
{
    fn handle_offline(self, server_addr: SocketAddr, config: Config) -> OfflineHandler<Self> {
        OfflineHandler {
            frame: Some(self),
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
        frame: Option<F>,
        state: State,
        server_addr: SocketAddr,
        config: Config,
    }
}

enum State {
    SendOpenConnectionRequest1(Packet<Frames>),
    WaitOpenConnectionReply1,
    SendOpenConnectionRequest2(Packet<Frames>),
    WaitOpenConnectionReply2,
}

impl<F> Future for OfflineHandler<F>
where
    F: Stream<Item = (Packet<FramesMut>, SocketAddr)>
        + Sink<(Packet<Frames>, SocketAddr), Error = CodecError>
        + Unpin,
{
    type Output = Result<impl Stream<Item = connected::Packet<FramesMut>>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let frame = this.frame.as_mut().unwrap();
        loop {
            match this.state {
                State::SendOpenConnectionRequest1(pack) => {
                    if let Err(err) = ready!(frame.poll_ready_unpin(cx)) {
                        debug!(
                            "[client] SendingOpenConnectionRequest1 poll_ready error: {err}, retrying"
                        );
                        continue;
                    }
                    if let Err(err) = frame.start_send_unpin((pack.clone(), *this.server_addr)) {
                        debug!(
                            "[client] SendingOpenConnectionRequest1 start_send error: {err}, retrying"
                        );
                        continue;
                    }
                    if let Err(err) = ready!(frame.poll_flush_unpin(cx)) {
                        debug!(
                            "[client] SendingOpenConnectionRequest1 poll_flush error: {err}, retrying"
                        );
                        continue;
                    }
                    *this.state = State::WaitOpenConnectionReply1;
                }
                State::WaitOpenConnectionReply1 => {
                    let Some((pack, addr)) = ready!(frame.poll_next_unpin(cx)) else {
                        return Poll::Ready(Err(Error::ConnectionClosed));
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
                    if let Err(err) = ready!(frame.poll_ready_unpin(cx)) {
                        debug!(
                            "[client] SendOpenConnectionRequest2 poll_ready error: {err}, retrying"
                        );
                        continue;
                    }
                    if let Err(err) = frame.start_send_unpin((pack.clone(), *this.server_addr)) {
                        debug!(
                            "[client] SendOpenConnectionRequest2 start_send error: {err}, retrying"
                        );
                        continue;
                    }
                    if let Err(err) = ready!(frame.poll_flush_unpin(cx)) {
                        debug!(
                            "[client] SendOpenConnectionRequest2 poll_flush error: {err}, retrying"
                        );
                        continue;
                    }
                    *this.state = State::WaitOpenConnectionReply2;
                }
                State::WaitOpenConnectionReply2 => {
                    let Some((pack, addr)) = ready!(frame.poll_next_unpin(cx)) else {
                        return Poll::Ready(Err(Error::ConnectionClosed));
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
                    return Poll::Ready(Ok(FilterConnected {
                        frame: this.frame.take().unwrap(),
                        server_addr: *this.server_addr,
                    }));
                }
            }
        }
    }
}

pin_project! {
    struct FilterConnected<F> {
        frame: F,
        server_addr: SocketAddr,
    }
}

impl<F> Stream for FilterConnected<F>
where
    F: Stream<Item = (Packet<FramesMut>, SocketAddr)> + Unpin,
{
    type Item = connected::Packet<FramesMut>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        loop {
            let Some((pack, addr)) = ready!(this.frame.poll_next_unpin(cx)) else {
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
