use std::pin::Pin;
use std::task::{ready, Context, Poll};

use flume::Sender;
use futures::{Stream, StreamExt};
use pin_project_lite::pin_project;

use crate::packet::connected::{self, AckOrNack, FrameSet};

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
