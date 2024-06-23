use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures::{Future, Sink, SinkExt, Stream, StreamExt};
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
            read: Some(self),
            write: Some(write),
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
        read: Option<I>,
        write: Option<O>,
        state: State,
        addr: SocketAddr,
    }
}

enum State {
    SendConnectionRequest(Option<FrameBody>),
    WaitConnectionRequestReply(Option<FrameBody>),
    SendNewIncomingConnection(FrameBody),
}

impl<I, O> Future for OnlineHandler<I, O>
where
    I: Stream<Item = FrameBody> + Unpin,
    O: Sink<FrameBody, Error = CodecError> + Sink<Message, Error = CodecError> + Unpin,
{
    type Output = Result<impl Stream<Item = Bytes> + Sink<Message, Error = Error>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let write = this.write.as_mut().unwrap();
        let read = this.read.as_mut().unwrap();
        loop {
            match this.state {
                State::SendConnectionRequest(pack) => {
                    if let Err(err) = ready!(SinkExt::<FrameBody>::poll_ready_unpin(write, cx)) {
                        debug!("[client] SendConnectionRequest poll_ready error: {err}, retrying");
                        continue;
                    }
                    if let Err(err) = write.start_send_unpin(pack.clone().unwrap()) {
                        debug!("[client] SendConnectionRequest start_send error: {err}, retrying");
                        continue;
                    }
                    if let Err(err) = ready!(SinkExt::<FrameBody>::poll_flush_unpin(write, cx)) {
                        debug!("[client] SendConnectionRequest poll_flush error: {err}, retrying");
                        continue;
                    }
                    *this.state = State::WaitConnectionRequestReply(pack.take());
                }
                State::WaitConnectionRequestReply(pack) => {
                    let Some(body) = ready!(read.poll_next_unpin(cx)) else {
                        return Poll::Ready(Err(Error::ConnectionClosed));
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
                            debug!("[client] got unexpected packet {body:?} on WaitConnectionRequestReply, fallback to SendConnectionRequest");
                            *this.state = State::SendConnectionRequest(pack.take());
                        }
                    }
                }
                State::SendNewIncomingConnection(pack) => {
                    if let Err(err) = ready!(SinkExt::<FrameBody>::poll_ready_unpin(write, cx)) {
                        debug!(
                            "[client] SendNewIncomingConnection poll_ready error: {err}, retrying"
                        );
                        continue;
                    }
                    if let Err(err) = write.start_send_unpin(pack.clone()) {
                        debug!(
                            "[client] SendNewIncomingConnection start_send error: {err}, retrying"
                        );
                        continue;
                    }
                    if let Err(err) = ready!(SinkExt::<FrameBody>::poll_flush_unpin(write, cx)) {
                        debug!(
                            "[client] SendNewIncomingConnection poll_flush error: {err}, retrying"
                        );
                        continue;
                    }
                    return Poll::Ready(Ok(FilterIO {
                        read: this.read.take().unwrap(),
                        write: this.write.take().unwrap(),
                        state: FilterIOState::Serving,
                    }));
                }
            }
        }
    }
}

pin_project! {
    struct FilterIO<I, O> {
        read: I,
        write: O,
        state: FilterIOState,
    }
}

enum FilterIOState {
    Serving,
    SendPing,
    Closed,
}

impl<I, O> Stream for FilterIO<I, O>
where
    I: Stream<Item = FrameBody> + Unpin,
    O: Sink<FrameBody, Error = CodecError> + Unpin,
{
    type Item = Bytes;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        loop {
            match this.state {
                FilterIOState::Serving => {
                    let Some(body) = ready!(this.read.poll_next_unpin(cx)) else {
                        *this.state = FilterIOState::Closed;
                        continue;
                    };
                    match body {
                        FrameBody::DisconnectNotification => *this.state = FilterIOState::Closed,
                        FrameBody::DetectLostConnections => *this.state = FilterIOState::SendPing,
                        FrameBody::User(data) => return Poll::Ready(Some(data)),
                        _ => {
                            debug!("[client] ignore packet {body:?} on Connected",);
                        }
                    }
                }
                FilterIOState::SendPing => {
                    if let Err(err) = ready!(this.write.poll_ready_unpin(cx)) {
                        debug!("[client] SendPing poll_ready error: {err}, retrying");
                        continue;
                    }
                    if let Err(err) = this.write.start_send_unpin(FrameBody::ConnectedPing {
                        client_timestamp: timestamp(),
                    }) {
                        debug!("[client] SendPing start_send error: {err}, retrying");
                        continue;
                    }
                    if let Err(err) = ready!(this.write.poll_flush_unpin(cx)) {
                        debug!("[client] SendPing poll_flush error: {err}, retrying");
                        continue;
                    }
                    *this.state = FilterIOState::Serving;
                }
                FilterIOState::Closed => return Poll::Ready(None),
            }
        }
    }
}

impl<I, O> Sink<Message> for FilterIO<I, O>
where
    O: Sink<Message, Error = CodecError> + Unpin,
{
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        if matches!(*this.state, FilterIOState::Closed) {
            return Poll::Ready(Err(Error::ConnectionClosed));
        }
        ready!(this.write.poll_ready_unpin(cx))?;
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        self.project().write.start_send_unpin(item)?;
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.project().write.poll_flush_unpin(cx))?;
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        ready!(this.write.poll_close_unpin(cx))?;
        *this.state = FilterIOState::Closed;
        Poll::Ready(Ok(()))
    }
}

fn timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
