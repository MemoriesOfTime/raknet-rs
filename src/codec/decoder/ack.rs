use std::pin::Pin;
use std::task::{ready, Context, Poll};

use flume::Sender;
use futures::{Stream, StreamExt};
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::{self, AckOrNack, FrameSet};

pin_project! {
    pub(crate) struct AckDispatcher<F> {
        #[pin]
        frame: F,
        ack_tx: Sender<AckOrNack>,
        nack_tx: Sender<AckOrNack>,
    }
}

pub(crate) trait AckDispatched: Sized {
    fn dispatch_recv_ack(
        self,
        ack_tx: Sender<AckOrNack>,
        nack_tx: Sender<AckOrNack>,
    ) -> AckDispatcher<Self>;
}

impl<F, S> AckDispatched for F
where
    F: Stream<Item = Result<connected::Packet<S>, CodecError>>,
{
    fn dispatch_recv_ack(
        self,
        ack_tx: Sender<AckOrNack>,
        nack_tx: Sender<AckOrNack>,
    ) -> AckDispatcher<Self> {
        AckDispatcher {
            frame: self,
            ack_tx,
            nack_tx,
        }
    }
}

impl<F, S> Stream for AckDispatcher<F>
where
    F: Stream<Item = Result<connected::Packet<S>, CodecError>>,
{
    type Item = Result<FrameSet<S>, CodecError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        loop {
            let Some(packet) = ready!(this.frame.poll_next_unpin(cx)?) else {
                return Poll::Ready(None);
            };
            // TODO: async send here?
            match packet {
                connected::Packet::FrameSet(frame_set) => return Poll::Ready(Some(Ok(frame_set))),
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
