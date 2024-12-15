use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap};
use std::sync::atomic::{self, AtomicBool};
use std::sync::Arc;
use std::time::Instant;

use async_channel::Sender;
use concurrent_queue::ConcurrentQueue;
use futures::Stream;
use log::{debug, warn};

use crate::packet::connected::{self, AckOrNack, FrameBody, FrameSet, FramesMut};
use crate::packet::unconnected;
use crate::utils::{u24, ConnId, Reactor};
use crate::{Peer, Role};

/// Shared link between stream and sink
pub(crate) type SharedLink = Arc<TransferLink>;

/// The `TransferLink` is an visitor structure that temporarily holds various types of transfer
/// link data, such as received `AckOrNack`, sequences of pending response sequence numbers, and
/// packets ready to send like `unconnected::Packet` and `FrameBody`.
/// It provides methods for compressing sequence numbers into `AckOrNack` as well as access to other
/// data within the link.
pub(crate) struct TransferLink {
    /// incoming ack with receive timestamp
    incoming_ack: ConcurrentQueue<(AckOrNack, Instant)>,
    /// incoming Nack packet.
    incoming_nack: ConcurrentQueue<AckOrNack>,
    /// the flag is set to `true` when the `OutgoingGuard` is closed,
    /// and there are still reliable packets awaiting ACK (needing resend).
    /// In this state, the close operation will sleep until an ACK is received to wake it,
    /// after which the flag will be reset to `false`.
    forward_waking: AtomicBool,

    /// pending ACK packets to be sent.
    outgoing_ack: parking_lot::Mutex<BinaryHeap<Reverse<u24>>>,
    /// pending NACK packets to be sent.
    outgoing_nack: parking_lot::Mutex<BTreeSet<u24>>,

    /// data related to unconnected packets awaiting processing.
    unconnected: ConcurrentQueue<unconnected::Packet>,
    /// data for the frame body that is yet to be handled.
    frame_body: ConcurrentQueue<FrameBody>,

    role: Role,
    peer: Peer,
}

impl TransferLink {
    pub(crate) fn new_arc(role: Role, peer: Peer) -> SharedLink {
        // avoiding ack flood, the overwhelming ack will be dropped and new ack will be displaced
        const MAX_ACK_BUFFER: usize = 4096;

        Arc::new(Self {
            incoming_ack: ConcurrentQueue::bounded(MAX_ACK_BUFFER),
            incoming_nack: ConcurrentQueue::bounded(MAX_ACK_BUFFER),
            forward_waking: AtomicBool::new(false),
            outgoing_ack: parking_lot::Mutex::new(BinaryHeap::with_capacity(MAX_ACK_BUFFER)),
            outgoing_nack: parking_lot::Mutex::new(BTreeSet::new()),
            unconnected: ConcurrentQueue::unbounded(),
            frame_body: ConcurrentQueue::unbounded(),
            role,
            peer,
        })
    }

    pub(crate) fn turn_on_waking(&self) {
        self.forward_waking.store(true, atomic::Ordering::Relaxed);
    }

    fn should_waking(&self) -> bool {
        self.forward_waking.load(atomic::Ordering::Relaxed)
    }

    pub(crate) fn turn_off_waking(&self) {
        self.forward_waking.store(false, atomic::Ordering::Relaxed);
    }

    pub(crate) fn incoming_ack(&self, records: AckOrNack) {
        if let Some((dropped, _)) = self
            .incoming_ack
            .force_push((records, Instant::now()))
            .unwrap()
        {
            warn!(
                "[{}] discard received ack {dropped:?} from {}, total count: {}",
                self.role,
                self.peer,
                dropped.total_cnt()
            );
        }
        // wake up after receiving an ack
        if self.should_waking() {
            let c_id = ConnId::new(self.role.guid(), self.peer.guid);
            let mut cnt = 0;
            for waker in Reactor::get().cancel_all_timers(c_id) {
                // safe to panic
                waker.wake();
                cnt += 1;
            }
            debug!(
                "[{}] wake up {cnt} wakers after receives ack on connection: {c_id:?}",
                self.role
            );
        }
    }

    pub(crate) fn incoming_nack(&self, records: AckOrNack) {
        if let Some(dropped) = self.incoming_nack.force_push(records).unwrap() {
            warn!(
                "[{}] discard received nack {dropped:?} from {}, total count: {}",
                self.role,
                self.peer,
                dropped.total_cnt()
            );
        }
    }

    pub(crate) fn send_unconnected(&self, packet: unconnected::Packet) {
        self.unconnected.push(packet).unwrap();
    }

    pub(crate) fn send_frame_body(&self, body: FrameBody) {
        self.frame_body.push(body).unwrap();
    }

    pub(crate) fn process_ack(&self) -> impl Iterator<Item = (AckOrNack, Instant)> + '_ {
        self.incoming_ack.try_iter()
    }

    pub(crate) fn process_nack(&self) -> impl Iterator<Item = AckOrNack> + '_ {
        self.incoming_nack.try_iter()
    }

    pub(crate) fn process_outgoing_ack(&self, mtu: u16) -> Option<AckOrNack> {
        AckOrNack::extend_from(self.outgoing_ack.lock().drain_sorted().map(|v| v.0), mtu)
    }

    pub(crate) fn process_outgoing_nack(&self, mtu: u16) -> Option<AckOrNack> {
        struct BatchRecv<'a> {
            guard: parking_lot::MutexGuard<'a, BTreeSet<u24>>,
        }

        impl<'a> Iterator for BatchRecv<'a> {
            type Item = u24;

            fn next(&mut self) -> Option<Self::Item> {
                self.guard.pop_first()
            }
        }

        AckOrNack::extend_from(
            BatchRecv {
                guard: self.outgoing_nack.lock(),
            },
            mtu,
        )
    }

    pub(crate) fn process_unconnected(&self) -> impl Iterator<Item = unconnected::Packet> + '_ {
        self.unconnected.try_iter()
    }

    pub(crate) fn process_frame_body(&self) -> impl Iterator<Item = FrameBody> + '_ {
        self.frame_body.try_iter()
    }

    pub(crate) fn outgoing_ack_empty(&self) -> bool {
        self.outgoing_ack.lock().is_empty()
    }

    pub(crate) fn outgoing_nack_empty(&self) -> bool {
        self.outgoing_nack.lock().is_empty()
    }

    pub(crate) fn unconnected_empty(&self) -> bool {
        self.unconnected.is_empty()
    }

    /// Return whether the frame body buffer is empty
    pub(crate) fn frame_body_empty(&self) -> bool {
        self.frame_body.is_empty()
    }
}

/// `Route` is an intermediary structure that wraps a `TransferLink`, providing the functionality to
/// `deliver` different types of data frames.
pub(crate) struct Route {
    /// `Route` create an asynchronous channel, splitting it into a sender and a
    ///   receiver, with the receiver being returned in the `new` method. This is the sender part
    /// of the   asynchronous channel.
    router_tx: Sender<FrameSet<FramesMut>>,
    link: SharedLink,
    // the next expected sequence number for incoming frames on this route
    seq_read: u24,
}

impl Route {
    pub(crate) fn new(link: SharedLink) -> (Self, impl Stream<Item = FrameSet<FramesMut>>) {
        let (router_tx, router_rx) = async_channel::unbounded();
        (
            Self {
                router_tx,
                link,
                seq_read: 0.into(),
            },
            router_rx,
        )
    }

    /// Deliver the packet to the corresponding route. Return false if the connection was dropped.
    pub(crate) fn deliver(&mut self, pack: connected::Packet<FramesMut>) -> bool {
        if self.router_tx.is_closed() {
            return false;
        }
        match pack {
            connected::Packet::FrameSet(frames) => {
                // TODO: use lock free concurrent queue to buffer the outgoing ack/nack to avoid
                // locking the mutex

                self.link.outgoing_ack.lock().push(Reverse(frames.seq_num));

                {
                    let mut nack = self.link.outgoing_nack.lock();
                    let seq_num = frames.seq_num;
                    nack.remove(&seq_num);
                    let pre_read = self.seq_read;
                    if pre_read <= seq_num {
                        self.seq_read = seq_num + 1;
                        for n in pre_read.to_u32()..seq_num.to_u32() {
                            nack.insert(n.into());
                        }
                    }
                }

                return self.router_tx.try_send(frames).is_ok();
            }
            connected::Packet::Ack(ack) => self.link.incoming_ack(ack),
            connected::Packet::Nack(nack) => self.link.incoming_nack(nack),
        };
        true
    }
}
