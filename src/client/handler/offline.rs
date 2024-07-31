use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use futures::{Future, Sink, SinkExt, Stream, StreamExt};
use pin_project_lite::pin_project;

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
        + Sink<(unconnected::Packet, SocketAddr), Error = io::Error>
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
        + Sink<(unconnected::Packet, SocketAddr), Error = io::Error>
        + Unpin,
{
    type Output = Result<impl Stream<Item = connected::Packet<FramesMut>>, io::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let frame = this.frame.as_mut().unwrap();
        loop {
            match this.state {
                State::SendOpenConnReq1(pack) => {
                    ready!(frame.poll_ready_unpin(cx))?;
                    frame.start_send_unpin((pack.clone(), *this.server_addr))?;
                    *this.state = State::SendOpenConnReq1Flush;
                }
                State::SendOpenConnReq1Flush => {
                    ready!(frame.poll_flush_unpin(cx))?;
                    *this.state = State::WaitOpenConnReply1;
                }
                State::WaitOpenConnReply1 => {
                    // TODO: Add timeout
                    let (pack, addr) = ready!(frame.poll_next_unpin(cx)).ok_or(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "connection reset by peer",
                    ))?;
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
                    ready!(frame.poll_ready_unpin(cx))?;
                    frame.start_send_unpin((pack.clone(), *this.server_addr))?;
                    *this.state = State::SendOpenConnReq2Flush;
                }
                State::SendOpenConnReq2Flush => {
                    ready!(frame.poll_flush_unpin(cx))?;
                    *this.state = State::WaitOpenConnReply2;
                }
                State::WaitOpenConnReply2 => {
                    // TODO: Add timeout
                    let (pack, addr) = ready!(frame.poll_next_unpin(cx)).ok_or(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "connection reset by peer",
                    ))?;
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
