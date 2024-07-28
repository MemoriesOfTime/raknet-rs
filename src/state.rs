//! State management for the connection.
//! Perform the 4-ways handshake for the connection close.
//! Reflect the operation in the APIs of Sink and Stream when the connection stops.

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};

use concurrent_queue::ConcurrentQueue;
use futures_core::Stream;
use log::warn;
use pin_project_lite::pin_project;

use crate::errors::Error;
use crate::io::Writer;
use crate::packet::connected::FrameBody;
use crate::Message;

enum OutgoingState {
    // before sending DisconnectNotification
    Connecting,
    DeliveredWait,
    FinWait,
    // after sending DisconnectNotification
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
        matches!(
            self,
            OutgoingState::Connecting | OutgoingState::DeliveredWait | OutgoingState::FinWait
        )
    }
}

/// Send close event when dropped.
pub(crate) struct CloseOnDrop {
    pub(crate) addr: SocketAddr,
    pub(crate) close_events: Arc<ConcurrentQueue<SocketAddr>>,
}

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.close_events
            .push(self.addr)
            .expect("closed events queue cannot be closed");
    }
}

impl CloseOnDrop {
    pub(crate) fn new(addr: SocketAddr, close_events: Arc<ConcurrentQueue<SocketAddr>>) -> Self {
        Self { addr, close_events }
    }
}

pin_project! {
    pub(crate) struct StateManager<F, S> {
        #[pin]
        frame: F,
        state: S,
        close_on_drop: Option<CloseOnDrop>,
    }
}

pub(crate) trait OutgoingStateManage: Sized {
    /// Manage the outgoing state of the connection.
    /// Take a sink of `FrameBody` and `Message` and return a sink of `FrameBody` and `Message`,
    /// mapping the `CodecError` to the `Error`.
    fn manage_outgoing_state(
        self,
        close_on_drop: Option<CloseOnDrop>,
    ) -> impl Writer<FrameBody> + Writer<Message>;
}

impl<F> OutgoingStateManage for F
where
    F: Writer<FrameBody> + Writer<Message>,
{
    fn manage_outgoing_state(
        self,
        close_on_drop: Option<CloseOnDrop>,
    ) -> impl Writer<FrameBody> + Writer<Message> {
        StateManager {
            frame: self,
            state: OutgoingState::Connecting,
            close_on_drop,
        }
    }
}

pub(crate) trait IncomingStateManage: Sized {
    /// Manage the incoming state of the connection.
    ///
    /// It will yield None when it receives the `DisconnectNotification`. And will continue to
    /// return None in the following.
    ///
    /// You have to repeatedly `poll_next` after receiving `DisconnectNotification` from
    /// the peer. This will ensure that the ack you sent to acknowledge the `DisconnectNotification`
    /// can be received by the the peer (i.e. ensuring that the the peer's `poll_close` call
    /// returns successfully).
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
            close_on_drop: None,
        }
    }
}

impl<F> Writer<FrameBody> for StateManager<F, OutgoingState>
where
    F: Writer<FrameBody>,
{
    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        if !self.state.before_finish() {
            return Poll::Ready(Err(Error::ConnectionClosed));
        }
        self.project().frame.poll_ready(cx)
    }

    fn feed(self: Pin<&mut Self>, item: FrameBody) {
        if !self.state.before_finish() {
            panic!("you cannot send after you receive ConnectionClosed in poll_ready");
        }
        self.project().frame.feed(item);
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        // flush is allowed after the connection is closed, it will deliver ack.
        self.project().frame.poll_flush(cx).map_err(Into::into)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        let mut this = self.project();
        if matches!(this.state, OutgoingState::Closed) {
            return Poll::Ready(Err(Error::ConnectionClosed));
        }
        loop {
            match this.state {
                OutgoingState::Connecting => {
                    *this.state = OutgoingState::DeliveredWait;
                }
                OutgoingState::DeliveredWait => {
                    // first wait all stales packets to receive by the peer
                    ready!(this.frame.as_mut().poll_delivered(cx)?);
                    *this.state = OutgoingState::FinWait;
                }
                OutgoingState::FinWait => {
                    // then send the DisconnectNotification
                    ready!(this.frame.as_mut().poll_ready(cx)?);
                    this.frame.as_mut().feed(FrameBody::DisconnectNotification);
                    *this.state = OutgoingState::CloseWait;
                }
                OutgoingState::CloseWait => {
                    // second wait the DisconnectNotification to receive by the peer
                    ready!(this.frame.as_mut().poll_close(cx)?);
                    *this.state = OutgoingState::Closed;
                }
                OutgoingState::Closed => {
                    // send close event
                    let _ = this.close_on_drop.take();
                    return Poll::Ready(Ok(()));
                }
            }
        }
    }
}

impl<F> Writer<Message> for StateManager<F, OutgoingState>
where
    F: Writer<FrameBody> + Writer<Message>,
{
    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Writer::<FrameBody>::poll_ready(self, cx)
    }

    fn feed(self: Pin<&mut Self>, item: Message) {
        self.project().frame.feed(item);
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Writer::<FrameBody>::poll_flush(self, cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Writer::<FrameBody>::poll_close(self, cx)
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
            // This happens when the router is dropped.
            warn!("router dropped before the connection is closed");
            *this.state = IncomingState::Closed;
            return Poll::Ready(None);
        };
        if matches!(body, FrameBody::DisconnectNotification) {
            // The peer no longer sends any data.
            *this.state = IncomingState::Closed;
            return Poll::Ready(None);
        }
        Poll::Ready(Some(body))
    }
}

#[cfg(test)]
mod test {
    use std::future::poll_fn;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll};

    use concurrent_queue::ConcurrentQueue;

    use crate::errors::Error;
    use crate::io::Writer;
    use crate::packet::connected::FrameBody;
    use crate::state::CloseOnDrop;
    use crate::Message;

    #[derive(Debug, Default)]
    struct DstSink {
        buf: Vec<FrameBody>,
    }

    impl Writer<FrameBody> for DstSink {
        fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
            Poll::Ready(Ok(()))
        }

        fn feed(mut self: Pin<&mut Self>, item: FrameBody) {
            self.buf.push(item);
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
            Poll::Ready(Ok(()))
        }
    }

    impl Writer<Message> for DstSink {
        fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
            Poll::Ready(Ok(()))
        }

        fn feed(self: Pin<&mut Self>, _item: Message) {}

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn test_goodbye_works() {
        let queue = Arc::new(ConcurrentQueue::unbounded());
        let addr = "0.0.0.0:0".parse().unwrap();
        let goodbye = super::StateManager {
            frame: DstSink::default(),
            state: crate::state::OutgoingState::Connecting,
            close_on_drop: Some(CloseOnDrop::new(addr, Arc::clone(&queue))),
        };

        tokio::pin!(goodbye);

        poll_fn(|cx| Writer::<FrameBody>::poll_close(goodbye.as_mut(), cx))
            .await
            .unwrap();
        assert_eq!(goodbye.frame.buf.len(), 1);
        assert!(matches!(
            goodbye.frame.buf[0],
            FrameBody::DisconnectNotification
        ));

        // close event was pushed
        assert_eq!(queue.pop().unwrap(), addr);
        assert!(queue.is_empty());

        // closed
        poll_fn(|cx| Writer::<FrameBody>::poll_ready(goodbye.as_mut(), cx))
            .await
            .unwrap_err();
    }
}
