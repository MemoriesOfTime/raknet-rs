//! State management for the connection.
//! Reflect the operation in the APIs of Sink and Stream when the connection stops.

use std::pin::Pin;
use std::task::{ready, Context, Poll};

use futures::{Sink, Stream};
use log::warn;
use pin_project_lite::pin_project;

use crate::errors::{CodecError, Error};
use crate::packet::connected::FrameBody;
use crate::Message;

enum OutgoingState {
    // before sending DisconnectNotification
    Connecting,
    FinWait,
    // after sending DisconnectNotification
    Fin,
    CloseWait,
    Closed,
}

enum IncomingState {
    Connecting,
    Closed,
}

impl OutgoingState {
    #[inline(always)]
    fn before_finish(&self) -> bool {
        matches!(self, OutgoingState::Connecting | OutgoingState::FinWait)
    }
}

pin_project! {
    pub(crate) struct StateManager<F, S> {
        #[pin]
        frame: F,
        state: S,
    }
}

pub(crate) trait OutgoingStateManage: Sized {
    /// Manage the outgoing state of the connection.
    /// Take a sink of `FrameBody` and `Message` and return a sink of `FrameBody` and `Message`,
    /// mapping the `CodecError` to the `Error`.
    fn manage_outgoing_state(
        self,
    ) -> impl Sink<FrameBody, Error = Error> + Sink<Message, Error = Error>;
}

impl<F> OutgoingStateManage for F
where
    F: Sink<FrameBody, Error = CodecError> + Sink<Message, Error = CodecError>,
{
    fn manage_outgoing_state(
        self,
    ) -> impl Sink<FrameBody, Error = Error> + Sink<Message, Error = Error> {
        StateManager {
            frame: self,
            state: OutgoingState::Connecting,
        }
    }
}

pub(crate) trait IncomingStateManage: Sized {
    /// Manage the incoming state of the connection.
    fn manage_incoming_state(self) -> impl Stream<Item = FrameBody>;
}

impl<F> IncomingStateManage for F
where
    F: Stream<Item = FrameBody>,
{
    fn manage_incoming_state(self) -> impl Stream<Item = FrameBody> {
        StateManager {
            frame: self,
            state: IncomingState::Connecting,
        }
    }
}

impl<F> Sink<FrameBody> for StateManager<F, OutgoingState>
where
    F: Sink<FrameBody, Error = CodecError>,
{
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if !self.state.before_finish() {
            return Poll::Ready(Err(Error::ConnectionClosed));
        }
        self.project().frame.poll_ready(cx).map_err(Into::into)
    }

    fn start_send(self: Pin<&mut Self>, item: FrameBody) -> Result<(), Self::Error> {
        if !self.state.before_finish() {
            return Err(Error::ConnectionClosed);
        }
        self.project().frame.start_send(item).map_err(Into::into)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if !self.state.before_finish() {
            return Poll::Ready(Err(Error::ConnectionClosed));
        }
        self.project().frame.poll_flush(cx).map_err(Into::into)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut this = self.project();
        if matches!(this.state, OutgoingState::Closed) {
            return Poll::Ready(Err(Error::ConnectionClosed));
        }
        loop {
            match this.state {
                OutgoingState::Connecting | OutgoingState::FinWait => {
                    *this.state = OutgoingState::FinWait;
                    ready!(this.frame.as_mut().poll_ready(cx)?);

                    this.frame
                        .as_mut()
                        .start_send(FrameBody::DisconnectNotification)?;
                    *this.state = OutgoingState::Fin;
                }
                OutgoingState::Fin => {
                    ready!(this.frame.as_mut().poll_flush(cx)?);
                    *this.state = OutgoingState::CloseWait;
                }
                OutgoingState::CloseWait => {
                    ready!(this.frame.as_mut().poll_close(cx)?);
                    *this.state = OutgoingState::Closed;
                }
                OutgoingState::Closed => return Poll::Ready(Ok(())),
            }
        }
    }
}

impl<F> Sink<Message> for StateManager<F, OutgoingState>
where
    F: Sink<FrameBody, Error = CodecError> + Sink<Message, Error = CodecError>,
{
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<FrameBody>::poll_ready(self, cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        self.project().frame.start_send(item)?;
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<FrameBody>::poll_flush(self, cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<FrameBody>::poll_close(self, cx)
    }
}

impl<F> Stream for StateManager<F, IncomingState>
where
    F: Stream<Item = FrameBody>,
{
    type Item = FrameBody;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        if matches!(this.state, IncomingState::Closed) {
            return Poll::Ready(None);
        }
        let Some(body) = ready!(this.frame.as_mut().poll_next(cx)) else {
            // this is weird, UDP will not be closed by the remote, but we regard it as closed
            warn!("Connection closed by the remote");
            *this.state = IncomingState::Closed;
            return Poll::Ready(None);
        };
        if matches!(body, FrameBody::DisconnectNotification) {
            *this.state = IncomingState::Closed;
            return Poll::Ready(None);
        }
        Poll::Ready(Some(body))
    }
}

#[cfg(test)]
mod test {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use futures::{Sink, SinkExt};

    use crate::errors::{CodecError, Error};
    use crate::packet::connected::FrameBody;
    use crate::Message;

    #[derive(Debug, Default)]
    struct DstSink {
        buf: Vec<FrameBody>,
    }

    impl Sink<FrameBody> for DstSink {
        type Error = CodecError;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(mut self: Pin<&mut Self>, item: FrameBody) -> Result<(), Self::Error> {
            self.buf.push(item);
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    impl Sink<Message> for DstSink {
        type Error = CodecError;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn test_goodbye_works() {
        let mut goodbye = super::StateManager {
            frame: DstSink::default(),
            state: crate::state::OutgoingState::Connecting,
        };
        SinkExt::<FrameBody>::close(&mut goodbye).await.unwrap();
        assert_eq!(goodbye.frame.buf.len(), 1);
        assert!(matches!(
            goodbye.frame.buf[0],
            FrameBody::DisconnectNotification
        ));

        let mut closed = SinkExt::<FrameBody>::close(&mut goodbye).await.unwrap_err();
        // closed
        assert!(matches!(closed, Error::ConnectionClosed));
        // No more DisconnectNotification
        assert_eq!(goodbye.frame.buf.len(), 1);

        // closed
        closed = SinkExt::<Message>::flush(&mut goodbye).await.unwrap_err();
        assert!(matches!(closed, Error::ConnectionClosed));
    }
}
