use std::collections::{BTreeMap, VecDeque};
use std::ops::Add;
use std::time::{Duration, Instant};

use crate::packet::connected::{AckOrNack, Frame, Frames, Record};
use crate::utils::u24;

// TODO: use RTTEstimator to get adaptive RTO
const RTO: Duration = Duration::from_millis(77);

struct ResendEntry {
    frames: Frames,
    expired_at: Instant,
}

pub(crate) struct ResendMap {
    map: BTreeMap<u24, ResendEntry>,
}

impl ResendMap {
    pub(crate) fn new() -> Self {
        Self {
            map: BTreeMap::new(),
        }
    }

    pub(crate) fn record(&mut self, seq_num: u24, frames: Frames) {
        self.map.insert(
            seq_num,
            ResendEntry {
                frames,
                expired_at: Instant::now().add(RTO),
            },
        );
    }

    pub(crate) fn on_ack(&mut self, ack: AckOrNack) {
        for record in ack.records {
            match record {
                Record::Range(start, end) => {
                    // TODO: optimized for range remove for sorted map
                    for i in start.to_u32()..end.to_u32() {
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
                    for i in start.to_u32()..end.to_u32() {
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
}
