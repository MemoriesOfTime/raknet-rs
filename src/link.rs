use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};

use flume::{Receiver, Sender};
use futures::{Stream, StreamExt};
use log::trace;
use pin_project_lite::pin_project;

use crate::packet::connected::{self, AckOrNack, Frame, FrameBody, FrameSet};
use crate::packet::unconnected;
use crate::resend_map::ResendMap;
use crate::utils::priority_mpsc::{self, Receiver as PriorityReceiver, Sender as PrioritySender};
use crate::utils::u24;
use crate::RoleContext;

/// Shared link between stream and sink
pub(crate) type SharedLink = Arc<TransferLink>;

/// Transfer data and task between stream and sink.
/// It is thread-safe under immutable reference
pub(crate) struct TransferLink {
    incoming_ack_tx: Sender<AckOrNack>,
    incoming_ack_rx: Receiver<AckOrNack>,

    incoming_nack_tx: Sender<AckOrNack>,
    incoming_nack_rx: Receiver<AckOrNack>,

    outgoing_ack_tx: PrioritySender<u24>,
    outgoing_ack_rx: PriorityReceiver<u24>,

    // TODO: nack channel should always be in order according to [`DeFragment::poll_next`], replace
    // it with flume
    outgoing_nack_tx: PrioritySender<u24>,
    outgoing_nack_rx: PriorityReceiver<u24>,

    unconnected_tx: Sender<unconnected::Packet>,
    unconnected_rx: Receiver<unconnected::Packet>,

    frame_body_tx: Sender<FrameBody>,
    frame_body_rx: Receiver<FrameBody>,

    role: RoleContext,
}

impl TransferLink {
    pub(crate) fn new_arc(role: RoleContext) -> SharedLink {
        let (incoming_ack_tx, incoming_ack_rx) = flume::unbounded();
        let (incoming_nack_tx, incoming_nack_rx) = flume::unbounded();

        let (outgoing_ack_tx, outgoing_ack_rx) = priority_mpsc::unbounded();
        let (outgoing_nack_tx, outgoing_nack_rx) = priority_mpsc::unbounded();

        let (unconnected_tx, unconnected_rx) = flume::unbounded();
        let (frame_body_tx, frame_body_rx) = flume::unbounded();

        Arc::new(Self {
            incoming_ack_tx,
            incoming_ack_rx,
            incoming_nack_tx,
            incoming_nack_rx,
            outgoing_ack_tx,
            outgoing_ack_rx,
            outgoing_nack_tx,
            outgoing_nack_rx,
            unconnected_tx,
            unconnected_rx,
            frame_body_tx,
            frame_body_rx,
            role,
        })
    }

    pub(crate) fn filter_incoming_ack<F, S>(
        self: &SharedLink,
        frame: F,
    ) -> impl Stream<Item = FrameSet<S>>
    where
        F: Stream<Item = connected::Packet<S>>,
    {
        IncomingAckFilter {
            frame,
            link: Arc::clone(self),
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

    pub(crate) fn send_unconnected(&self, packet: unconnected::Packet) {
        self.unconnected_tx.send(packet).unwrap();
    }

    pub(crate) fn send_frame_body(&self, body: FrameBody) {
        self.frame_body_tx.send(body).unwrap();
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

    pub(crate) fn poll_unconnected(&self) -> Option<unconnected::Packet> {
        self.unconnected_rx.try_recv().ok()
    }

    pub(crate) fn poll_frame_body(&self) -> Option<FrameBody> {
        self.frame_body_rx.try_recv().ok()
    }

    // Return whether the flush buffer is empty
    pub(crate) fn flush_empty(&self) -> bool {
        self.outgoing_ack_rx.is_empty()
            && self.outgoing_nack_rx.is_empty()
            && self.unconnected_rx.is_empty()
    }

    /// Return whether the frame body buffer is empty
    pub(crate) fn frame_body_empty(&self) -> bool {
        self.frame_body_rx.is_empty()
    }
}

pin_project! {
    // IncomingAckFilter delivers all acknowledgement packets into transfer link, mapping
    // connected::Packet into FrameSet
    pub(crate) struct IncomingAckFilter<F> {
        #[pin]
        frame: F,
        link: SharedLink
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
                connected::Packet::Ack(records) => this.link.incoming_ack(records),
                connected::Packet::Nack(records) => this.link.incoming_nack(records),
            };
        }
    }
}
