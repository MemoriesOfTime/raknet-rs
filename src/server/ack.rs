use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::Bytes;
use flume::{Receiver, Sender};
use futures::{Sink, Stream, StreamExt};
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::{self, AckOrNack, FrameSet, Frames};

pin_project! {
    pub(super) struct IncomingAck<F> {
        #[pin]
        frame: F,
        ack_tx: Sender<AckOrNack>,
        nack_tx: Sender<AckOrNack>,
    }
}

pub(super) trait HandleIncomingAck: Sized {
    fn handle_incoming_ack(
        self,
        ack_tx: Sender<AckOrNack>,
        nack_tx: Sender<AckOrNack>,
    ) -> IncomingAck<Self>;
}

impl<F, S> HandleIncomingAck for F
where
    F: Stream<Item = connected::Packet<S>>,
{
    fn handle_incoming_ack(
        self,
        ack_tx: Sender<AckOrNack>,
        nack_tx: Sender<AckOrNack>,
    ) -> IncomingAck<Self> {
        IncomingAck {
            frame: self,
            ack_tx,
            nack_tx,
        }
    }
}

impl<F, S> Stream for IncomingAck<F>
where
    F: Stream<Item = connected::Packet<S>>,
{
    type Item = FrameSet<S>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        loop {
            let Some(packet) = ready!(this.frame.poll_next_unpin(cx)) else {
                return Poll::Ready(None);
            };
            // TODO: async send here?
            match packet {
                connected::Packet::FrameSet(frame_set) => return Poll::Ready(Some(frame_set)),
                connected::Packet::Ack(ack) => {
                    this.ack_tx.send(ack).expect("ack_rx must not be dropped");
                }
                connected::Packet::Nack(nack) => {
                    this.nack_tx.send(nack).expect("ack_rx must not be dropped");
                }
            };
        }
    }
}

pin_project! {
    struct OutgoingAck<F> {
        #[pin]
        frame: F,
        incoming_ack_rx: Receiver<AckOrNack>,
        incoming_nack_rx: Receiver<AckOrNack>,
        outgoing_ack_rx: Receiver<AckOrNack>,
        outgoing_nack_rx: Receiver<AckOrNack>,
        window: SlidingWindow,
    }
}

struct SlidingWindow;

impl<F> Sink<Frames<Bytes>> for OutgoingAck<F> {
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }

    fn start_send(self: Pin<&mut Self>, item: Frames<Bytes>) -> Result<(), Self::Error> {
        todo!()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }
}
