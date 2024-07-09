use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::packet::connected::{AckOrNack, Frame, Frames, Record};
use crate::utils::u24;

// TODO: use RTTEstimator to get adaptive RTO
const RTO: Duration = Duration::from_millis(77);

struct ResendEntry {
    frames: Frames,
    expired_at: Instant,
}

pub(crate) struct ResendMap {
    // TODO: maybe use rbtree to optimize the performance
    map: BTreeMap<u24, ResendEntry>,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl ResendMap {
    pub(crate) fn new() -> Self {
        Self {
            map: BTreeMap::new(),
            waker: Arc::new(Mutex::new(None)),
        }
        // TODO: spawn a thread to wake up the task when the entry is expired
    }

    pub(crate) fn record(&mut self, seq_num: u24, frames: Frames) {
        self.map.insert(
            seq_num,
            ResendEntry {
                frames,
                expired_at: Instant::now() + RTO,
            },
        );
    }

    pub(crate) fn on_ack(&mut self, ack: AckOrNack) {
        for record in ack.records {
            match record {
                Record::Range(start, end) => {
                    // TODO: optimized for range remove for sorted map
                    for i in start.to_u32()..=end.to_u32() {
                        self.map.remove(&i.into());
                    }
                }
                Record::Single(seq_num) => {
                    self.map.remove(&seq_num);
                }
            }
        }
    }

    pub(crate) fn on_nack_into(&mut self, nack: AckOrNack, buffer: &mut VecDeque<Frame>) {
        for record in nack.records {
            match record {
                Record::Range(start, end) => {
                    for i in start.to_u32()..=end.to_u32() {
                        if let Some(entry) = self.map.remove(&i.into()) {
                            buffer.extend(entry.frames);
                        }
                    }
                }
                Record::Single(seq_num) => {
                    if let Some(entry) = self.map.remove(&seq_num) {
                        buffer.extend(entry.frames);
                    }
                }
            }
        }
    }

    /// `poll_stales_into` polls stale frames into buffer and remove the expired entries
    pub(crate) fn poll_stales_into(&mut self, buffer: &mut VecDeque<Frame>) {
        let now = Instant::now();
        while let Some(entry) = self.map.first_entry() {
            // ordered by seq_num, the large seq_num has the large next_send
            // TODO: is it a good optimization?
            if now < entry.get().expired_at {
                break;
            }
            buffer.extend(entry.remove().frames);
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// `poll_wait` suspends the task when the resend map needs to wait for the next resend
    pub(crate) fn poll_wait(&self, cx: &mut Context<'_>) -> Poll<()> {
        let Some((_, entry)) = self.map.first_key_value() else {
            return Poll::Ready(());
        };
        let wait = entry.expired_at.saturating_duration_since(Instant::now());
        if wait.is_zero() {
            return Poll::Ready(());
        }

        // TODO: optimize this code

        let old_waker = self.waker.lock().replace(cx.waker().clone());
        // wake up the old waker if the task is moved
        if let Some(old_waker) = old_waker {
            old_waker.wake();
        }
        let waker_ref = self.waker.clone();

        std::thread::spawn(move || {
            std::thread::sleep(wait);
            if let Some(waker) = waker_ref.lock().take() {
                waker.wake();
            }
        });

        Poll::Pending
    }
}
