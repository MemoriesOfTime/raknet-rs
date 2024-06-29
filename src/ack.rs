use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::AtomicBool;

use bytes::Bytes;
use flume::{Receiver, Sender};
use log::trace;
use minstant::Instant;

use crate::packet::connected::{AckOrNack, Frame, Frames, Record};
use crate::utils::priority_mpsc::{Receiver as PriorityReceiver, Sender as PrioritySender};
use crate::utils::u24;
use crate::RoleContext;

pub(crate) struct Acknowledgement {
    incoming_ack_tx: Sender<AckOrNack>,
    incoming_ack_rx: Receiver<AckOrNack>,

    incoming_nack_tx: Sender<AckOrNack>,
    incoming_nack_rx: Receiver<AckOrNack>,

    outgoing_ack_tx: PrioritySender<u24>,
    outgoing_ack_rx: PriorityReceiver<u24>,

    outgoing_nack_tx: PrioritySender<u24>,
    outgoing_nack_rx: PriorityReceiver<u24>,

    rb: AtomicBool,

    role: RoleContext,
}

impl Acknowledgement {
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

    pub(crate) fn filter_resending(
        &self,
        resending: &mut BTreeMap<u24, (Frames<Bytes>, Instant)>,
        buffer: &mut VecDeque<Frame<Bytes>>,
    ) {
        for ack in self.incoming_ack_rx.try_iter() {
            trace!(
                "[{}] receive ack {ack:?}, total count: {}",
                self.role,
                ack.total_cnt()
            );
            for record in ack.records {
                match record {
                    Record::Range(start, end) => {
                        for i in start.to_u32()..end.to_u32() {
                            // TODO: optimized for range remove for btree map
                            resending.remove(&i.into());
                        }
                    }
                    Record::Single(seq_num) => {
                        resending.remove(&seq_num);
                    }
                }
            }
        }
        for nack in self.incoming_nack_rx.try_iter() {
            trace!(
                "[{}] receive nack {nack:?}, total count: {}",
                self.role,
                nack.total_cnt()
            );
            for record in nack.records {
                match record {
                    Record::Range(start, end) => {
                        for i in start.to_u32()..end.to_u32() {
                            if let Some((set, _)) = resending.remove(&i.into()) {
                                buffer.extend(set);
                            }
                        }
                    }
                    Record::Single(seq_num) => {
                        if let Some((set, _)) = resending.remove(&seq_num) {
                            buffer.extend(set);
                        }
                    }
                }
            }
        }
    }

    // Poll outgoing ack or nack with round robin
    pub(crate) fn poll_outgoing(&self, mtu: u16) -> Option<AckOrNack> {
        if self.rb.load(std::sync::atomic::Ordering::Relaxed) {
            self.rb.store(false, std::sync::atomic::Ordering::Relaxed);
            AckOrNack::extend_from(self.outgoing_ack_rx.recv_batch(), mtu)
                .or_else(|| AckOrNack::extend_from(self.outgoing_nack_rx.recv_batch(), mtu))
        } else {
            self.rb.store(true, std::sync::atomic::Ordering::Relaxed);
            AckOrNack::extend_from(self.outgoing_nack_rx.recv_batch(), mtu)
                .or_else(|| AckOrNack::extend_from(self.outgoing_ack_rx.recv_batch(), mtu))
        }
    }
}
