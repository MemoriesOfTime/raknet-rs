use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};

use flume::{Receiver, Sender};
use futures::{Stream, StreamExt};
use log::trace;
use pin_project_lite::pin_project;

use crate::packet::connected::{self, AckOrNack, Frame, FrameSet};
use crate::resend_map::ResendMap;
use crate::utils::priority_mpsc::{self, Receiver as PriorityReceiver, Sender as PrioritySender};
use crate::utils::u24;
use crate::RoleContext;

/// Shared acknowledgement handler
pub(crate) type SharedAck = Arc<Acknowledgement>;

pub(crate) struct Acknowledgement {
    incoming_ack_tx: Sender<AckOrNack>,
    incoming_ack_rx: Receiver<AckOrNack>,

    incoming_nack_tx: Sender<AckOrNack>,
    incoming_nack_rx: Receiver<AckOrNack>,

    outgoing_ack_tx: PrioritySender<u24>,
    outgoing_ack_rx: PriorityReceiver<u24>,

    outgoing_nack_tx: PrioritySender<u24>,
    outgoing_nack_rx: PriorityReceiver<u24>,

    role: RoleContext,
}

impl Acknowledgement {
    pub(crate) fn new_arc(role: RoleContext) -> SharedAck {
        let (incoming_ack_tx, incoming_ack_rx) = flume::unbounded();
        let (incoming_nack_tx, incoming_nack_rx) = flume::unbounded();

        let (outgoing_ack_tx, outgoing_ack_rx) = priority_mpsc::unbounded();
        let (outgoing_nack_tx, outgoing_nack_rx) = priority_mpsc::unbounded();
        Arc::new(Self {
            incoming_ack_tx,
            incoming_ack_rx,
            incoming_nack_tx,
            incoming_nack_rx,
            outgoing_ack_tx,
            outgoing_ack_rx,
            outgoing_nack_tx,
            outgoing_nack_rx,
            role,
        })
    }

    pub(crate) fn filter_incoming_ack<F, S>(
        self: &SharedAck,
        frame: F,
    ) -> impl Stream<Item = FrameSet<S>>
    where
        F: Stream<Item = connected::Packet<S>>,
    {
        IncomingAckFilter {
            frame,
            ack: Arc::clone(self),
        }
    }

    pub(crate) fn incoming_ack(&self, records: AckOrNack) {
        self.incoming_ack_tx.send(records).unwrap();
    }

    pub(crate) fn incoming_nack(&self, records: AckOrNack) {
        self.incoming_nack_tx.send(records).unwrap();
    }

    pub(crate) fn outgoing_ack(&self, seq_num: u24) {
        self.outgoing_ack_tx.send(seq_num);
    }

    pub(crate) fn outgoing_nack(&self, seq_num: u24) {
        self.outgoing_nack_tx.send(seq_num);
    }

    pub(crate) fn outgoing_nack_batch(&self, t: impl IntoIterator<Item = u24>) {
        self.outgoing_nack_tx.send_batch(t);
    }

    // Clear all acknowledged frames
    pub(crate) fn poll_ack(&self, resend: &mut ResendMap) {
        for ack in self.incoming_ack_rx.try_iter() {
            trace!(
                "[{}] receive ack {ack:?}, total count: {}",
                self.role,
                ack.total_cnt()
            );
            resend.on_ack(ack);
        }
    }

    /// Push all missing frames into buffer
    /// Notice this method should be called after serval invoking of [`Acknowledgement::poll_ack`].
    /// Otherwise, some packets that do not need to be resent may be sent.
    /// As for how many times to invoke [`Acknowledgement::poll_ack`] before this, it depends.
    pub(crate) fn poll_resend(&self, resend: &mut ResendMap, buffer: &mut VecDeque<Frame>) {
        for nack in self.incoming_nack_rx.try_iter() {
            trace!(
                "[{}] receive nack {nack:?}, total count: {}",
                self.role,
                nack.total_cnt()
            );
            resend.on_nack_into(nack, buffer);
        }
    }

    pub(crate) fn poll_outgoing_ack(&self, mtu: u16) -> Option<AckOrNack> {
        AckOrNack::extend_from(self.outgoing_ack_rx.recv_batch(), mtu)
    }

    pub(crate) fn poll_outgoing_nack(&self, mtu: u16) -> Option<AckOrNack> {
        AckOrNack::extend_from(self.outgoing_nack_rx.recv_batch(), mtu)
    }

    // Return whether the outgoing buffer is empty
    pub(crate) fn empty(&self) -> bool {
        self.outgoing_ack_rx.is_empty() && self.outgoing_nack_rx.is_empty()
    }
}

pin_project! {
    // IncomingGuard delivers all acknowledgement packets into outgoing guard, mapping
    // connected::Packet into FrameSet
    pub(crate) struct IncomingAckFilter<F> {
        #[pin]
        frame: F,
        ack: SharedAck
    }
}

impl<F, S> Stream for IncomingAckFilter<F>
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
            match packet {
                connected::Packet::FrameSet(frame_set) => return Poll::Ready(Some(frame_set)),
                connected::Packet::Ack(records) => this.ack.incoming_ack(records),
                connected::Packet::Nack(records) => this.ack.incoming_nack(records),
            };
        }
    }
}
