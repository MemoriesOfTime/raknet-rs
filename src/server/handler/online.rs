use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::ops::Deref;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use flume::Sender;
use futures::{Sink, Stream};
use log::{debug, error};
use pin_project_lite::pin_project;

use crate::errors::{CodecError, Error};
use crate::packet::connected::FrameBody;
use crate::packet::unconnected;
use crate::server::Message;

pub(crate) trait HandleOnline: Sized {
    fn handle_online<O, RO>(
        self,
        write: O,
        raw_write: RO,
        client_addr: SocketAddr,
        server_guid: u64,
        drop_notifier: Sender<SocketAddr>,
    ) -> OnlineHandler<Self, O, RO>
    where
        O: Sink<FrameBody, Error = CodecError>,
        RO: Sink<unconnected::Packet, Error = CodecError>;
}

impl<F> HandleOnline for F
where
    F: Stream<Item = FrameBody>,
{
    fn handle_online<O, RO>(
        self,
        write: O,
        raw_write: RO,
        client_addr: SocketAddr,
        server_guid: u64,
        drop_notifier: Sender<SocketAddr>,
    ) -> OnlineHandler<Self, O, RO>
    where
        O: Sink<FrameBody, Error = CodecError>,
        RO: Sink<unconnected::Packet, Error = CodecError>,
    {
        OnlineHandler {
            state: State::HandshakePhase1,
            server_guid,
            client_addr: AddrDropGuard {
                client_addr,
                drop_notifier,
            },
            read: self,
            write,
            raw_write,
        }
    }
}

pin_project! {
    pub(crate) struct OnlineHandler<I, O, RO> {
        state: State,
        server_guid: u64,
        client_addr: AddrDropGuard,
        #[pin]
        read: I,
        #[pin]
        write: O,
        #[pin]
        raw_write: RO,
    }
}

struct AddrDropGuard {
    client_addr: SocketAddr,
    drop_notifier: Sender<SocketAddr>,
}

impl Deref for AddrDropGuard {
    type Target = SocketAddr;

    fn deref(&self) -> &Self::Target {
        &self.client_addr
    }
}

impl Drop for AddrDropGuard {
    fn drop(&mut self) {
        if self.drop_notifier.try_send(self.client_addr).is_err() {
            error!(
                "[online] cannot send IO for {} drop notification to drop_notifier",
                self.client_addr
            );
            return;
        }
        debug!("[online] connection from {} is dropped", self.client_addr);
    }
}

enum State {
    HandshakePhase1,
    SendAccept(Option<FrameBody>),
    SendAcceptFlush,
    SendFailed(Option<unconnected::Packet>),
    SendFailedFlush,
    HandshakePhase2,
    Connected,
    SendPong(Option<FrameBody>),
    SendPongFlush,
    Closed,
}

impl<I, O, RO> Stream for OnlineHandler<I, O, RO>
where
    I: Stream<Item = FrameBody>,
    O: Sink<FrameBody, Error = CodecError>,
    RO: Sink<unconnected::Packet, Error = CodecError>,
{
    type Item = Bytes;

    #[allow(clippy::cognitive_complexity)] // not bad
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            match this.state {
                State::HandshakePhase1 => {
                    let Some(body) = ready!(this.read.as_mut().poll_next(cx)) else {
                        *this.state = State::Closed;
                        continue;
                    };

                    match body {
                        FrameBody::ConnectionRequest {
                            request_timestamp,
                            use_encryption,
                            ..
                        } => {
                            if use_encryption {
                                *this.state = State::SendFailed(Some(
                                    unconnected::Packet::ConnectionRequestFailed {
                                        magic: (),
                                        server_guid: *this.server_guid,
                                    },
                                ));
                            } else {
                                let system_addr = if this.client_addr.is_ipv6() {
                                    SocketAddr::new(
                                        std::net::IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0)),
                                        0,
                                    )
                                } else {
                                    SocketAddr::new(
                                        std::net::IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
                                        0,
                                    )
                                };
                                *this.state =
                                    State::SendAccept(Some(FrameBody::ConnectionRequestAccepted {
                                        client_address: **this.client_addr,
                                        system_index: 0,
                                        system_addresses: [system_addr; 20],
                                        request_timestamp,
                                        accepted_timestamp: timestamp(),
                                    }));
                            }
                        }
                        _ => {
                            debug!("[online] ignore packet {body:?} on HandshakePhase1",);
                        }
                    };
                }
                State::SendAccept(pack) => {
                    if let Err(err) = ready!(this.write.as_mut().poll_ready(cx)) {
                        debug!("[online] SendAccept poll_ready error: {err}, fallback to HandshakePhase1");
                        *this.state = State::HandshakePhase1;
                        continue;
                    }
                    if let Err(err) = this.write.as_mut().start_send(pack.take().unwrap()) {
                        debug!("[online] SendAccept start_send error: {err}, fallback to HandshakePhase1");
                        *this.state = State::HandshakePhase1;
                        continue;
                    }
                    *this.state = State::SendAcceptFlush;
                }
                State::SendAcceptFlush => {
                    if let Err(err) = ready!(this.write.as_mut().poll_flush(cx)) {
                        debug!(
                            "[online] SendAcceptFlush poll_flush error: {err}, fallback to HandshakePhase1"
                        );
                        *this.state = State::HandshakePhase1;
                        continue;
                    }
                    *this.state = State::HandshakePhase2;
                }
                State::SendFailed(pack) => {
                    if let Err(err) = ready!(this.raw_write.as_mut().poll_ready(cx)) {
                        debug!("[online] SendFailed poll_ready error: {err}, fallback to HandshakePhase1");
                        *this.state = State::HandshakePhase1;
                        continue;
                    }
                    if let Err(err) = this.raw_write.as_mut().start_send(pack.take().unwrap()) {
                        debug!("[online] SendFailed start_send error: {err}, fallback to HandshakePhase1");
                        *this.state = State::HandshakePhase1;
                        continue;
                    }
                    *this.state = State::SendFailedFlush;
                }
                State::SendFailedFlush => {
                    if let Err(err) = ready!(this.raw_write.as_mut().poll_flush(cx)) {
                        debug!("[online] SendFailedFlush poll_flush error: {err}");
                    }
                    *this.state = State::HandshakePhase1;
                }
                State::HandshakePhase2 => {
                    let Some(body) = ready!(this.read.as_mut().poll_next(cx)) else {
                        *this.state = State::Closed;
                        continue;
                    };
                    match body {
                        FrameBody::NewIncomingConnection { .. } => {
                            debug!("[online] connections finished handshake");
                            *this.state = State::Connected;
                        }
                        _ => {
                            debug!("[online] ignore packet {body:?} on HandshakePhase2");
                        }
                    };
                }
                State::Connected => {
                    let Some(body) = ready!(this.read.as_mut().poll_next(cx)) else {
                        *this.state = State::Closed;
                        continue;
                    };
                    match body {
                        FrameBody::ConnectedPing { client_timestamp } => {
                            *this.state = State::SendPong(Some(FrameBody::ConnectedPong {
                                client_timestamp,
                                server_timestamp: timestamp(),
                            }));
                        }
                        FrameBody::DisconnectNotification => {
                            *this.state = State::Closed;
                        }
                        FrameBody::User(data) => return Poll::Ready(Some(data)),
                        _ => {
                            debug!("[online] ignore packet {body:?} on Connected",);
                        }
                    }
                }
                State::SendPong(pack) => {
                    if let Err(err) = ready!(this.write.as_mut().poll_ready(cx)) {
                        debug!("[online] SendPong poll_ready error: {err}");
                        *this.state = State::Connected;
                        continue;
                    }
                    if let Err(err) = this.write.as_mut().start_send(pack.take().unwrap()) {
                        debug!("[online] SendPong start_send error: {err}");
                        *this.state = State::Connected;
                        continue;
                    }
                    *this.state = State::SendPongFlush;
                }
                State::SendPongFlush => {
                    if let Err(err) = ready!(this.write.as_mut().poll_flush(cx)) {
                        debug!("[online] SendPongFlush poll_flush error: {err}");
                    }
                    *this.state = State::Connected;
                }
                State::Closed => return Poll::Ready(None),
            }
        }
    }
}

impl<I, O, RO> Sink<Message> for OnlineHandler<I, O, RO>
where
    O: Sink<Message, Error = CodecError>,
{
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        if matches!(*this.state, State::Closed) {
            return Poll::Ready(Err(Error::ConnectionClosed));
        }
        ready!(this.write.poll_ready(cx))?;
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        self.project().write.start_send(item)?;
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.project().write.poll_flush(cx))?;
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        ready!(this.write.poll_close(cx))?;
        *this.state = State::Closed;
        Poll::Ready(Ok(()))
    }
}

fn timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
