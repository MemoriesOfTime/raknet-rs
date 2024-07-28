use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use futures::Sink;
use futures_core::Stream;
use log::debug;
use pin_project_lite::pin_project;

use crate::errors::{CodecError, Error};
use crate::packet::connected::{self, FramesMut};
use crate::packet::{unconnected, Packet};
use crate::RoleContext;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Config {
    pub(crate) mtu: u16,
    pub(crate) client_guid: u64,
    pub(crate) protocol_version: u8,
}

pin_project! {
    pub(crate) struct OfflineHandler<F> {
        frame: Option<F>,
        state: State,
        server_addr: SocketAddr,
        config: Config,
        role: RoleContext,
    }
}

impl<F> OfflineHandler<F>
where
    F: Stream<Item = (Packet<FramesMut>, SocketAddr)>
        + Sink<(unconnected::Packet, SocketAddr), Error = CodecError>
        + Unpin,
{
    pub(crate) fn new(frame: F, server_addr: SocketAddr, config: Config) -> Self {
        Self {
            frame: Some(frame),
            state: State::SendOpenConnReq1(unconnected::Packet::OpenConnectionRequest1 {
                magic: (),
                protocol_version: config.protocol_version,
                mtu: config.mtu,
            }),
            server_addr,
            role: RoleContext::Client {
                guid: config.client_guid,
            },
            config,
        }
    }
}

enum State {
    SendOpenConnReq1(unconnected::Packet),
    SendOpenConnReq1Flush,
    WaitOpenConnReply1,
    SendOpenConnReq2(unconnected::Packet),
    SendOpenConnReq2Flush,
    WaitOpenConnReply2,
}

impl<F> Future for OfflineHandler<F>
where
    F: Stream<Item = (Packet<FramesMut>, SocketAddr)>
        + Sink<(unconnected::Packet, SocketAddr), Error = CodecError>
        + Unpin,
{
    type Output = Result<impl Stream<Item = connected::Packet<FramesMut>>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let mut frame = Pin::new(this.frame.as_mut().unwrap());
        loop {
            match this.state {
                State::SendOpenConnReq1(pack) => {
                    if let Err(err) = ready!(frame.as_mut().poll_ready(cx)) {
                        debug!(
                            "[{}] SendingOpenConnectionRequest1 poll_ready error: {err}, retrying",
                            this.role
                        );
                        continue;
                    }
                    if let Err(err) = frame.as_mut().start_send((pack.clone(), *this.server_addr)) {
                        debug!(
                            "[{}] SendingOpenConnectionRequest1 start_send error: {err}, retrying",
                            this.role
                        );
                        continue;
                    }
                    *this.state = State::SendOpenConnReq1Flush;
                }
                State::SendOpenConnReq1Flush => {
                    if let Err(err) = ready!(frame.as_mut().poll_flush(cx)) {
                        debug!(
                            "[{}] SendingOpenConnectionRequest1 poll_flush error: {err}, retrying",
                            this.role
                        );
                        continue;
                    }
                    *this.state = State::WaitOpenConnReply1;
                }
                State::WaitOpenConnReply1 => {
                    // TODO: Add timeout
                    let Some((pack, addr)) = ready!(frame.as_mut().poll_next(cx)) else {
                        return Poll::Ready(Err(Error::ConnectionClosed));
                    };
                    if addr != *this.server_addr {
                        continue;
                    }
                    let next = match pack {
                        Packet::Unconnected(unconnected::Packet::OpenConnectionReply1 {
                            mtu,
                            ..
                        }) => unconnected::Packet::OpenConnectionRequest2 {
                            magic: (),
                            server_address: *this.server_addr,
                            mtu,
                            client_guid: this.config.client_guid,
                        },
                        _ => continue,
                    };
                    *this.state = State::SendOpenConnReq2(next);
                }
                State::SendOpenConnReq2(pack) => {
                    if let Err(err) = ready!(frame.as_mut().poll_ready(cx)) {
                        debug!(
                            "[{}] SendOpenConnectionRequest2 poll_ready error: {err}, retrying",
                            this.role
                        );
                        continue;
                    }
                    if let Err(err) = frame.as_mut().start_send((pack.clone(), *this.server_addr)) {
                        debug!(
                            "[{}] SendOpenConnectionRequest2 start_send error: {err}, retrying",
                            this.role
                        );
                        continue;
                    }
                    *this.state = State::SendOpenConnReq2Flush;
                }
                State::SendOpenConnReq2Flush => {
                    if let Err(err) = ready!(frame.as_mut().poll_flush(cx)) {
                        debug!(
                            "[{}] SendOpenConnectionRequest2 poll_flush error: {err}, retrying",
                            this.role
                        );
                        continue;
                    }
                    *this.state = State::WaitOpenConnReply2;
                }
                State::WaitOpenConnReply2 => {
                    // TODO: Add timeout
                    let Some((pack, addr)) = ready!(frame.as_mut().poll_next(cx)) else {
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
            let Some((pack, addr)) = ready!(Pin::new(&mut *this.frame).poll_next(cx)) else {
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
