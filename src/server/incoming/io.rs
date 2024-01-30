use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::ops::Deref;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use flume::Sender;
use futures::{Sink, Stream, StreamExt};
use pin_project_lite::pin_project;
use tracing::{debug, error, warn};

use crate::errors::{CodecError, Error};
use crate::packet::connected::{FrameBody, Reliability};
use crate::packet::unconnected;
use crate::server::{IOpts, Message};

pin_project! {
    pub(crate) struct IOImpl<I, O, RO> {
        pub(super) state: IOState,
        pub(super) default_reliability: Reliability,
        pub(super) default_order_channel: u8,
        pub(super) client_addr: AddrDropGuard,
        pub(super) server_guid: u64,
        #[pin]
        pub(super) write: O,
        #[pin]
        pub(super) raw_write: RO,
        #[pin]
        pub(super) read: I,
    }
}

pub(super) struct AddrDropGuard {
    pub(super) client_addr: SocketAddr,
    pub(super) drop_notifier: Sender<SocketAddr>,
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
                "cannot send IO for {} drop notification to drop_notifier",
                self.client_addr
            );
        }
    }
}

pub(super) enum HandShakeState {
    Phase1,
    SendAccept(Option<FrameBody>),
    SendAcceptFlush,
    SendFailed(Option<unconnected::Packet>),
    SendFailedFlush,
    Phase2,
}

pub(super) enum IOState {
    HandShaking(HandShakeState),
    Serve,
    Closed,
}

impl<I, O, RO> Stream for IOImpl<I, O, RO>
where
    I: Stream<Item = FrameBody>,
    O: Sink<FrameBody>,
    RO: Sink<unconnected::Packet>,
{
    type Item = Bytes;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            match this.state {
                IOState::HandShaking(state) => match state {
                    HandShakeState::Phase1 => {
                        let Some(body) = ready!(this.read.as_mut().poll_next(cx)) else {
                            *this.state = IOState::Closed;
                            return Poll::Ready(None);
                        };
                        match body {
                            FrameBody::ConnectionRequest {
                                sever_guid,
                                request_timestamp,
                                use_encryption,
                            } => {
                                if sever_guid != *this.server_guid || use_encryption {
                                    *state = HandShakeState::SendFailed(Some(
                                        unconnected::Packet::ConnectionRequestFailed {
                                            magic: (),
                                            server_guid: *this.server_guid,
                                        },
                                    ));
                                } else {
                                    let timestamp = timestamp();
                                    let system_addr = if this.client_addr.is_ipv6() {
                                        SocketAddr::new(
                                            std::net::IpAddr::V6(Ipv6Addr::new(
                                                0, 0, 0, 0, 0, 0, 0, 0,
                                            )),
                                            0,
                                        )
                                    } else {
                                        SocketAddr::new(
                                            std::net::IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
                                            0,
                                        )
                                    };
                                    *state = HandShakeState::SendAccept(Some(
                                        FrameBody::ConnectionRequestAccepted {
                                            client_address: **this.client_addr,
                                            system_index: 0,
                                            system_addresses: [system_addr; 20],
                                            request_timestamp,
                                            accepted_timestamp: timestamp,
                                        },
                                    ));
                                }
                            }
                            _ => {
                                debug!(
                                    "ignore packet {body:?} from {} on HandShakeState::Phase1",
                                    **this.client_addr
                                );
                                continue;
                            }
                        };
                    }
                    HandShakeState::SendAccept(resp) => {
                        if ready!(this.write.as_mut().poll_ready(cx)).is_err() {
                            debug!("HandShakeState::SendAccept poll_ready error, fallback to HandShakeState::Phase1");
                            *state = HandShakeState::Phase1;
                            continue;
                        }
                        if this
                            .write
                            .as_mut()
                            .start_send(resp.take().unwrap())
                            .is_err()
                        {
                            debug!("HandShakeState::SendAccept start_send error, fallback to HandShakeState::Phase1");
                            *state = HandShakeState::Phase1;
                            continue;
                        }
                        *state = HandShakeState::SendAcceptFlush;
                    }
                    HandShakeState::SendFailed(resp) => {
                        if ready!(this.raw_write.as_mut().poll_ready(cx)).is_err() {
                            debug!("HandShakeState::SendFailed poll_ready error, fallback to HandShakeState::Phase1");
                            *state = HandShakeState::Phase1;
                            continue;
                        }
                        if this
                            .raw_write
                            .as_mut()
                            .start_send(resp.take().unwrap())
                            .is_err()
                        {
                            debug!("HandShakeState::SendFailed start_send error, fallback to HandShakeState::Phase1");
                            *state = HandShakeState::Phase1;
                            continue;
                        }
                        *state = HandShakeState::SendFailedFlush;
                    }
                    HandShakeState::SendFailedFlush => {
                        let _ig = ready!(this.raw_write.as_mut().poll_flush(cx));
                        *state = HandShakeState::Phase1;
                    }
                    HandShakeState::SendAcceptFlush => {
                        if ready!(this.write.as_mut().poll_flush(cx)).is_err() {
                            debug!("HandShakeState::SendAcceptFlush poll_flush error, fallback to HandShakeState::Phase1");
                            *state = HandShakeState::Phase1;
                            continue;
                        }
                        *state = HandShakeState::Phase2;
                    }
                    HandShakeState::Phase2 => {
                        let Some(body) = ready!(this.read.as_mut().poll_next(cx)) else {
                            *this.state = IOState::Closed;
                            return Poll::Ready(None);
                        };
                        match body {
                            FrameBody::NewIncomingConnection { .. } => {
                                *this.state = IOState::Serve;
                            }
                            _ => {
                                warn!("ignore packet {body:?} from non-handshake connections");
                                continue;
                            }
                        };
                    }
                },
                IOState::Serve => {
                    let conn = this.read.as_mut().filter_map(|body| async move {
                        match body {
                            FrameBody::User(data) => Some(data),
                            _ => None,
                        }
                    });
                    tokio::pin!(conn);
                    if let Some(data) = ready!(conn.poll_next(cx)) {
                        return Poll::Ready(Some(data));
                    }
                    *this.state = IOState::Closed;
                }
                IOState::Closed => return Poll::Ready(None),
            }
        }
    }
}

impl<I, O, RO> Sink<Message> for IOImpl<I, O, RO>
where
    O: Sink<Message, Error = CodecError>,
{
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        if matches!(*this.state, IOState::Closed) {
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
        *this.state = IOState::Closed;
        Poll::Ready(Ok(()))
    }
}

impl<I, O, RO> Sink<Bytes> for IOImpl<I, O, RO>
where
    O: Sink<Message, Error = CodecError>,
{
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<Message>::poll_ready(self, cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        let msg = Message::new(self.default_reliability, self.default_order_channel, item);
        Sink::<Message>::start_send(self, msg)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<Message>::poll_flush(self, cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<Message>::poll_close(self, cx)
    }
}

impl<I, O, RO> IOpts for IOImpl<I, O, RO> {
    fn set_default_reliability(&mut self, reliability: Reliability) {
        self.default_reliability = reliability;
    }

    fn get_default_reliability(&self) -> Reliability {
        self.default_reliability
    }

    fn set_default_order_channel(&mut self, order_channel: u8) {
        self.default_order_channel = order_channel;
    }

    fn get_default_order_channel(&self) -> u8 {
        self.default_order_channel
    }
}

fn timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
