use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures::{Sink, Stream};
use log::debug;
use pin_project_lite::pin_project;

use crate::errors::{CodecError, Error};
use crate::packet::connected::FrameBody;
use crate::Message;

pub(crate) trait HandleOnline: Sized {
    fn handle_online<O>(
        self,
        write: O,
        addr: SocketAddr,
        client_guid: u64,
    ) -> OnlineHandler<Self, O>
    where
        O: Sink<FrameBody, Error = CodecError>;
}

impl<F> HandleOnline for F
where
    F: Stream<Item = FrameBody>,
{
    fn handle_online<O>(
        self,
        write: O,
        addr: SocketAddr,
        client_guid: u64,
    ) -> OnlineHandler<Self, O>
    where
        O: Sink<FrameBody, Error = CodecError>,
    {
        OnlineHandler {
            read: self,
            write,
            state: State::SendConnectionRequest(Some(FrameBody::ConnectionRequest {
                client_guid,
                request_timestamp: timestamp(),
                use_encryption: false,
            })),
            addr,
        }
    }
}

pin_project! {
    pub(crate) struct OnlineHandler<I, O> {
        #[pin]
        read: I,
        #[pin]
        write: O,
        state: State,
        addr: SocketAddr,
    }
}

enum State {
    SendConnectionRequest(Option<FrameBody>),
    WaitConnectionRequestReply(Option<FrameBody>),
    SendNewIncomingConnection(FrameBody),
    Connected,
    SendPing,
    Closed,
}

impl<I, O> Stream for OnlineHandler<I, O>
where
    I: Stream<Item = FrameBody>,
    O: Sink<FrameBody, Error = CodecError>,
{
    type Item = Bytes;

    #[allow(clippy::cognitive_complexity)]
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            match this.state {
                State::SendConnectionRequest(pack) => {
                    if let Err(err) = ready!(this.write.as_mut().poll_ready(cx)) {
                        debug!("[online] SendConnectionRequest poll_ready error: {err}, retrying");
                        continue;
                    }
                    if let Err(err) = this.write.as_mut().start_send(pack.clone().unwrap()) {
                        debug!("[online] SendConnectionRequest start_send error: {err}, retrying");
                        continue;
                    }
                    if let Err(err) = ready!(this.write.as_mut().poll_flush(cx)) {
                        debug!("[online] SendConnectionRequest poll_flush error: {err}, retrying");
                        continue;
                    }
                    *this.state = State::WaitConnectionRequestReply(pack.take());
                }
                State::WaitConnectionRequestReply(pack) => {
                    let Some(body) = ready!(this.read.as_mut().poll_next(cx)) else {
                        *this.state = State::Closed;
                        continue;
                    };
                    match body {
                        FrameBody::ConnectionRequestAccepted {
                            system_addresses,
                            accepted_timestamp,
                            ..
                        } => {
                            *this.state = State::SendNewIncomingConnection(
                                FrameBody::NewIncomingConnection {
                                    server_address: *this.addr,
                                    system_addresses,
                                    request_timestamp: timestamp(),
                                    accepted_timestamp,
                                },
                            );
                        }
                        _ => {
                            debug!("[online] got unexpected packet {body:?} on WaitConnectionRequestReply, fallback to SendConnectionRequest");
                            *this.state = State::SendConnectionRequest(pack.take());
                        }
                    }
                }
                State::SendNewIncomingConnection(pack) => {
                    if let Err(err) = ready!(this.write.as_mut().poll_ready(cx)) {
                        debug!(
                            "[online] SendNewIncomingConnection poll_ready error: {err}, retrying"
                        );
                        continue;
                    }
                    if let Err(err) = this.write.as_mut().start_send(pack.clone()) {
                        debug!(
                            "[online] SendNewIncomingConnection start_send error: {err}, retrying"
                        );
                        continue;
                    }
                    if let Err(err) = ready!(this.write.as_mut().poll_flush(cx)) {
                        debug!(
                            "[online] SendNewIncomingConnection poll_flush error: {err}, retrying"
                        );
                        continue;
                    }
                    *this.state = State::Connected;
                }
                State::Connected => {
                    let Some(body) = ready!(this.read.as_mut().poll_next(cx)) else {
                        *this.state = State::Closed;
                        continue;
                    };
                    match body {
                        FrameBody::DisconnectNotification => *this.state = State::Closed,
                        FrameBody::DetectLostConnections => *this.state = State::SendPing,
                        FrameBody::User(data) => return Poll::Ready(Some(data)),
                        _ => {
                            debug!("[online] ignore packet {body:?} on Connected",);
                        }
                    }
                }
                State::SendPing => {
                    if let Err(err) = ready!(this.write.as_mut().poll_ready(cx)) {
                        debug!("[online] SendPing poll_ready error: {err}, retrying");
                        continue;
                    }
                    if let Err(err) = this.write.as_mut().start_send(FrameBody::ConnectedPing {
                        client_timestamp: timestamp(),
                    }) {
                        debug!("[online] SendPing start_send error: {err}, retrying");
                        continue;
                    }
                    if let Err(err) = ready!(this.write.as_mut().poll_flush(cx)) {
                        debug!("[online] SendPing poll_flush error: {err}, retrying");
                        continue;
                    }
                    *this.state = State::Connected;
                }
                State::Closed => return Poll::Ready(None),
            }
        }
    }
}

impl<I, O> Sink<Message> for OnlineHandler<I, O>
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
