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
        // TODO: optimize this code

        let old_waker = self.waker.lock().replace(cx.waker().clone());
        // wake up the old waker if the task is moved
        if let Some(old_waker) = old_waker {
            old_waker.wake();
        }

        let Some((_, entry)) = self.map.first_key_value() else {
            return Poll::Ready(());
        };
        let wait = entry.expired_at.saturating_duration_since(Instant::now());
        if wait.is_zero() {
            return Poll::Ready(());
        }

        let waker_ref = self.waker.clone();

        // TODO: I know this is stupid, we should spawn a daemon thread to wake up the task. But
        // poll_wait is hardly called now.
        std::thread::spawn(move || {
            std::thread::sleep(wait);
            if let Some(waker) = waker_ref.lock().take() {
                waker.wake();
            }
        });

        Poll::Pending
    }
}

#[cfg(test)]
mod test {
    use std::collections::VecDeque;
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;

    use bytes::Bytes;

    use super::ResendMap;
    use crate::packet::connected::{AckOrNack, Flags, Frame};

    #[test]
    fn test_resend_map_works() {
        let mut map = ResendMap::new();
        map.record(0.into(), vec![]);
        map.record(1.into(), vec![]);
        map.record(2.into(), vec![]);
        map.record(3.into(), vec![]);
        assert!(!map.is_empty());
        map.on_ack(AckOrNack::extend_from([0, 1, 2, 3].into_iter().map(Into::into), 100).unwrap());
        assert!(map.is_empty());

        map.record(
            4.into(),
            vec![Frame {
                flags: Flags::new(crate::packet::connected::Reliability::Unreliable, false),
                reliable_frame_index: None,
                seq_frame_index: None,
                ordered: None,
                fragment: None,
                body: Bytes::from_static(b"1"),
            }],
        );
        map.record(
            5.into(),
            vec![
                Frame {
                    flags: Flags::new(crate::packet::connected::Reliability::Unreliable, false),
                    reliable_frame_index: None,
                    seq_frame_index: None,
                    ordered: None,
                    fragment: None,
                    body: Bytes::from_static(b"2"),
                },
                Frame {
                    flags: Flags::new(crate::packet::connected::Reliability::Unreliable, false),
                    reliable_frame_index: None,
                    seq_frame_index: None,
                    ordered: None,
                    fragment: None,
                    body: Bytes::from_static(b"3"),
                },
            ],
        );
        let mut buffer = VecDeque::default();
        map.on_nack_into(
            AckOrNack::extend_from([4, 5].into_iter().map(Into::into), 100).unwrap(),
            &mut buffer,
        );
        assert!(map.is_empty());
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.pop_front().unwrap().body, Bytes::from_static(b"1"));
        assert_eq!(buffer.pop_front().unwrap().body, Bytes::from_static(b"2"));
        assert_eq!(buffer.pop_front().unwrap().body, Bytes::from_static(b"3"));
    }

    #[test]
    fn test_resend_map_stales() {
        const TEST_RTO: Duration = Duration::from_millis(100);

        let mut map = ResendMap::new();
        map.record(0.into(), vec![]);
        map.record(1.into(), vec![]);
        map.record(2.into(), vec![]);
        std::thread::sleep(TEST_RTO);
        map.record(3.into(), vec![]);
        let mut buffer = VecDeque::default();
        map.poll_stales_into(&mut buffer);
        assert_eq!(map.map.len(), 1);
    }

    #[tokio::test]
    async fn test_resend_map_poll_wait() {
        const TEST_RTO: Duration = Duration::from_millis(100);

        let mut map = ResendMap::new();
        map.record(0.into(), vec![]);
        std::thread::sleep(TEST_RTO);
        map.record(1.into(), vec![]);
        map.record(2.into(), vec![]);
        map.record(3.into(), vec![]);

        let mut buffer = VecDeque::default();

        let res = map.poll_wait(&mut Context::from_waker(Waker::noop()));
        assert!(matches!(res, Poll::Ready(_)));

        map.poll_stales_into(&mut buffer);
        assert_eq!(map.map.len(), 3);

        std::future::poll_fn(|cx| map.poll_wait(cx)).await;
        map.poll_stales_into(&mut buffer);
        assert!(map.map.len() < 3);
    }
}
