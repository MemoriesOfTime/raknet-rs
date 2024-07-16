use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};

use concurrent_queue::ConcurrentQueue;
use futures::{Stream, StreamExt};
use log::{trace, warn};
use pin_project_lite::pin_project;

use crate::packet::connected::{self, AckOrNack, Frame, FrameBody, FrameSet};
use crate::packet::unconnected;
use crate::resend_map::ResendMap;
use crate::utils::u24;
use crate::RoleContext;

/// Shared link between stream and sink
pub(crate) type SharedLink = Arc<TransferLink>;

/// Transfer data and task between stream and sink.
/// It is thread-safe under immutable reference
pub(crate) struct TransferLink {
    incoming_ack: ConcurrentQueue<AckOrNack>,
    incoming_nack: ConcurrentQueue<AckOrNack>,

    outgoing_ack: parking_lot::Mutex<BinaryHeap<Reverse<u24>>>,
    // TODO: nack channel should always be in order according to [`DeFragment::poll_next`], replace
    // it with ConcurrentQueue if we cannot find a way to break the order
    outgoing_nack: parking_lot::Mutex<BinaryHeap<Reverse<u24>>>,

    unconnected: ConcurrentQueue<unconnected::Packet>,
    frame_body: ConcurrentQueue<FrameBody>,

    role: RoleContext,
}

/// Pop priority queue while holding the lock
struct BatchRecv<'a, T> {
    guard: parking_lot::MutexGuard<'a, BinaryHeap<Reverse<T>>>,
}

impl<'a, T> BatchRecv<'a, T> {
    fn new(guard: parking_lot::MutexGuard<'a, BinaryHeap<Reverse<T>>>) -> Self {
        Self { guard }
    }
}

impl<'a, T: Ord> Iterator for BatchRecv<'a, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.guard.pop().map(|v| v.0)
    }
}

impl TransferLink {
    pub(crate) fn new_arc(role: RoleContext) -> SharedLink {
        // avoiding ack flood, the overwhelming ack will be dropped and new ack will be displaced
        const MAX_ACK_BUFFER: usize = 1024;

        Arc::new(Self {
            incoming_ack: ConcurrentQueue::bounded(MAX_ACK_BUFFER),
            incoming_nack: ConcurrentQueue::bounded(MAX_ACK_BUFFER),
            outgoing_ack: parking_lot::Mutex::new(BinaryHeap::with_capacity(MAX_ACK_BUFFER)),
            outgoing_nack: parking_lot::Mutex::new(BinaryHeap::with_capacity(MAX_ACK_BUFFER)),
            unconnected: ConcurrentQueue::unbounded(),
            frame_body: ConcurrentQueue::unbounded(),
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
        if let Some(dropped) = self.incoming_ack.force_push(records).unwrap() {
            warn!(
                "[{}] discard received ack {dropped:?}, total count: {}",
                self.role,
                dropped.total_cnt()
            );
        }
    }

    pub(crate) fn incoming_nack(&self, records: AckOrNack) {
        if let Some(dropped) = self.incoming_nack.force_push(records).unwrap() {
            warn!(
                "[{}] discard received nack {dropped:?}, total count: {}",
                self.role,
                dropped.total_cnt()
            );
        }
    }

    pub(crate) fn outgoing_ack(&self, seq_num: u24) {
        self.outgoing_ack.lock().push(Reverse(seq_num));
    }

    pub(crate) fn outgoing_nack(&self, seq_num: u24) {
        self.outgoing_nack.lock().push(Reverse(seq_num));
    }

    pub(crate) fn outgoing_nack_batch(&self, t: impl IntoIterator<Item = u24>) {
        self.outgoing_nack.lock().extend(t.into_iter().map(Reverse));
    }

    pub(crate) fn send_unconnected(&self, packet: unconnected::Packet) {
        self.unconnected.push(packet).unwrap();
    }

    pub(crate) fn send_frame_body(&self, body: FrameBody) {
        self.frame_body.push(body).unwrap();
    }

    // Clear all acknowledged frames
    pub(crate) fn process_ack(&self, resend: &mut ResendMap) {
        for ack in self.incoming_ack.try_iter() {
            trace!(
                "[{}] receive ack {ack:?}, total count: {}",
                self.role,
                ack.total_cnt()
            );
            resend.on_ack(ack);
        }
    }

    /// Push all missing frames into buffer
    /// Notice this method should be called after serval invoking of
    /// [`Acknowledgement::process_ack`]. Otherwise, some packets that do not need to be resent
    /// may be sent. As for how many times to invoke [`Acknowledgement::process_ack`] before
    /// this, it depends.
    pub(crate) fn process_resend(&self, resend: &mut ResendMap, buffer: &mut VecDeque<Frame>) {
        for nack in self.incoming_nack.try_iter() {
            trace!(
                "[{}] receive nack {nack:?}, total count: {}",
                self.role,
                nack.total_cnt()
            );
            resend.on_nack_into(nack, buffer);
        }
    }

    pub(crate) fn process_outgoing_ack(&self, mtu: u16) -> Option<AckOrNack> {
        AckOrNack::extend_from(BatchRecv::new(self.outgoing_ack.lock()), mtu)
    }

    pub(crate) fn process_outgoing_nack(&self, mtu: u16) -> Option<AckOrNack> {
        AckOrNack::extend_from(BatchRecv::new(self.outgoing_nack.lock()), mtu)
    }

    pub(crate) fn process_unconnected(&self) -> impl Iterator<Item = unconnected::Packet> + '_ {
        self.unconnected.try_iter()
    }

    pub(crate) fn process_frame_body(&self) -> impl Iterator<Item = FrameBody> + '_ {
        self.frame_body.try_iter()
    }

    // Return whether the flush buffer is empty
    pub(crate) fn flush_empty(&self) -> bool {
        self.outgoing_ack.lock().is_empty()
            && self.outgoing_nack.lock().is_empty()
            && self.unconnected.is_empty()
    }

    /// Return whether the frame body buffer is empty
    pub(crate) fn frame_body_empty(&self) -> bool {
        self.frame_body.is_empty()
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
