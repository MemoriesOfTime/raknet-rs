//! State management for the connection.

use std::pin::Pin;
use std::task::{ready, Context, Poll};

use futures::Sink;
use pin_project_lite::pin_project;

use crate::errors::{CodecError, Error};
use crate::packet::connected::FrameBody;
use crate::Message;

enum State {
    // before sending DisconnectNotification
    Connecting,
    FinWait,
    // after sending DisconnectNotification
    Fin,
    CloseWait,
    Closed,
}

impl State {
    #[inline(always)]
    fn before_finish(&self) -> bool {
        matches!(self, State::Connecting | State::FinWait)
    }
}

pin_project! {
    pub(crate) struct StateManager<F> {
        #[pin]
        frame: F,
        state: State,
    }
}

pub(crate) trait StateManage: Sized {
    /// Manage the state of the connection.
    /// Take a sink of `FrameBody` and `Message` and return a sink of `FrameBody` and `Message`,
    /// mapping the `CodecError` to the `Error`.
    fn manage_state(self) -> impl Sink<FrameBody, Error = Error> + Sink<Message, Error = Error>;
}

impl<F> StateManage for F
where
    F: Sink<FrameBody, Error = CodecError> + Sink<Message, Error = CodecError>,
{
    fn manage_state(self) -> impl Sink<FrameBody, Error = Error> + Sink<Message, Error = Error> {
        StateManager {
            frame: self,
            state: State::Connecting,
        }
    }
}

impl<F> Sink<FrameBody> for StateManager<F>
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
        if matches!(this.state, State::Closed) {
            return Poll::Ready(Err(Error::ConnectionClosed));
        }
        loop {
            match this.state {
                State::Connecting | State::FinWait => {
                    *this.state = State::FinWait;
                    ready!(this.frame.as_mut().poll_ready(cx)?);

                    this.frame
                        .as_mut()
                        .start_send(FrameBody::DisconnectNotification)?;
                    *this.state = State::Fin;
                }
                State::Fin => {
                    ready!(this.frame.as_mut().poll_flush(cx)?);
                    *this.state = State::CloseWait;
                }
                State::CloseWait => {
                    ready!(this.frame.as_mut().poll_close(cx)?);
                    *this.state = State::Closed;
                }
                State::Closed => return Poll::Ready(Ok(())),
            }
        }
    }
}

impl<F> Sink<Message> for StateManager<F>
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

#[cfg(test)]
mod test {
    // TODO: test

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
            state: crate::state::State::Connecting,
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
