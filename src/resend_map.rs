use std::collections::{HashMap, VecDeque};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use log::trace;

use crate::packet::connected::{AckOrNack, Frame, Frames, Record};
use crate::utils::u24;
use crate::RoleContext;

// TODO: use RTTEstimator to get adaptive RTO
const RTO: Duration = Duration::from_secs(1);

struct ResendEntry {
    frames: Option<Frames>,
    expired_at: Instant,
}

pub(crate) struct ResendMap {
    map: HashMap<u24, ResendEntry>,
    role: RoleContext,
    last_record_expired_at: Instant,
}

impl ResendMap {
    pub(crate) fn new(role: RoleContext) -> Self {
        Self {
            map: HashMap::new(),
            role,
            last_record_expired_at: Instant::now(),
        }
    }

    pub(crate) fn record(&mut self, seq_num: u24, frames: Frames) {
        self.map.insert(
            seq_num,
            ResendEntry {
                frames: Some(frames),
                expired_at: Instant::now() + RTO,
            },
        );
    }

    pub(crate) fn on_ack(&mut self, ack: AckOrNack) {
        for record in ack.records {
            match record {
                Record::Range(start, end) => {
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
                            buffer.extend(entry.frames.unwrap());
                        }
                    }
                }
                Record::Single(seq_num) => {
                    if let Some(entry) = self.map.remove(&seq_num) {
                        buffer.extend(entry.frames.unwrap());
                    }
                }
            }
        }
    }

    /// `process_stales` collect all stale frames into buffer and remove the expired entries
    pub(crate) fn process_stales(&mut self, buffer: &mut VecDeque<Frame>) {
        let now = Instant::now();
        if now < self.last_record_expired_at {
            // probably no stale entries, skip scanning the map
            return;
        }
        // find the first expired_at larger than now
        let mut min_expired_at = now + RTO;
        self.map.retain(|_, entry| {
            if entry.expired_at <= now {
                buffer.extend(entry.frames.take().unwrap());
                false
            } else {
                min_expired_at = min_expired_at.min(entry.expired_at);
                true
            }
        });
        debug_assert!(min_expired_at > now);
        trace!(
            "[{}]: process stales, {} entries left, next expired at {:?}",
            self.role,
            self.map.len(),
            min_expired_at
        );
        self.last_record_expired_at = min_expired_at;
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// `poll_wait` suspends the task when the resend map needs to wait for the next resend
    pub(crate) fn poll_wait(&self, cx: &mut Context<'_>) -> Poll<()> {
        let expired_at;
        let seq_num;
        let now = Instant::now();
        if let Some((seq, entry)) = self.map.iter().min_by_key(|(_, entry)| entry.expired_at)
            && entry.expired_at > now
        {
            expired_at = entry.expired_at;
            seq_num = *seq;
        } else {
            return Poll::Ready(());
        }
        trace!(
            "[{}]: wait for resend seq_num {} within {:?}",
            self.role,
            seq_num,
            expired_at - now
        );
        reactor::Reactor::get().insert_timer(expired_at, seq_num, self.role.guid(), cx.waker());
        Poll::Pending
    }
}

/// Specialized timer reactor for resend map
pub(crate) mod reactor {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::OnceLock;
    use std::task::Waker;
    use std::time::{Duration, Instant};
    use std::{mem, panic, thread};

    use crate::utils::u24;

    /// A unique sequence number with a global unique ID.
    /// This is used to identify a timer across the different peers.
    #[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Default)]
    struct UniqueSeq {
        seq_num: u24,
        guid: u64,
    }

    /// A simple timer reactor.
    ///
    /// There is only one global instance of this type, accessible by [`Reactor::get()`].
    pub(crate) struct Reactor {
        /// Inner state of the timer reactor.
        inner: parking_lot::Mutex<ReactorInner>,
        /// A condition variable to notify the reactor of new timers.
        cond: parking_lot::Condvar,
    }

    /// Inner state of the timer reactor.
    struct ReactorInner {
        /// An ordered map of registered timers.
        ///
        /// Timers are in the order in which they fire. The `UniqueSeq` in this type is relative to
        /// the timer and plays a role in a unique ID for same timeout. The `Waker`
        /// represents the task awaiting the timer.
        timers: BTreeMap<(Instant, UniqueSeq), Waker>,
        /// A mapping of unique seq num to their respective `Instant`s.
        ///
        /// This is used to cancel timers with a given sequence number.
        mapping: HashMap<UniqueSeq, Instant>,
    }

    impl Reactor {
        pub(crate) fn get() -> &'static Reactor {
            static REACTOR: OnceLock<Reactor> = OnceLock::new();

            fn main_loop() {
                let reactor = Reactor::get();
                loop {
                    reactor.process_timers();
                }
            }

            REACTOR.get_or_init(|| {
                // Spawn the daemon thread to motivate the reactor.
                thread::Builder::new()
                    .name("timer-reactor".to_string())
                    .spawn(main_loop)
                    .expect("cannot spawn timer-reactor thread");

                Reactor {
                    inner: parking_lot::Mutex::new(ReactorInner {
                        timers: BTreeMap::new(),
                        mapping: HashMap::new(),
                    }),
                    cond: parking_lot::Condvar::new(),
                }
            })
        }

        /// Locks the reactor for exclusive access.
        pub(crate) fn lock(&self) -> ReactorLock<'_> {
            ReactorLock {
                inner: self.inner.lock(),
                cond: &self.cond,
            }
        }

        /// Registers a timer in the reactor.
        ///
        /// Returns the inserted timer's ID.
        pub(crate) fn insert_timer(&self, when: Instant, seq_num: u24, guid: u64, waker: &Waker) {
            let mut guard = self.inner.lock();
            let unique_seq = UniqueSeq { seq_num, guid };
            guard.mapping.insert(unique_seq, when);
            guard.timers.insert((when, unique_seq), waker.clone());

            // Notify that a timer has been inserted.
            self.cond.notify_one();
        }

        /// Processes ready timers and waits for the next timer to be inserted.
        fn process_timers(&self) {
            let mut inner = self.inner.lock();

            let now = Instant::now();

            // Split timers into ready and pending timers.
            //
            // Careful to split just *after* `now`, so that a timer set for exactly `now` is
            // considered ready.
            let pending = inner
                .timers
                .split_off(&(now + Duration::from_nanos(1), UniqueSeq::default()));
            let ready = mem::replace(&mut inner.timers, pending);

            for ((_, seq_num), waker) in ready {
                inner.mapping.remove(&seq_num);
                // TODO: wake up maybe slow down the reactor
                // Don't let a panicking waker blow everything up.
                panic::catch_unwind(|| waker.wake()).ok();
            }

            // Calculate the duration until the next event.
            let dur = inner
                .timers
                .keys()
                .next()
                .map(|(when, _)| when.saturating_duration_since(now));

            if let Some(dur) = dur {
                self.cond.wait_for(&mut inner, dur);
            } else {
                self.cond.wait(&mut inner);
            }
        }
    }

    pub(crate) struct ReactorLock<'a> {
        inner: parking_lot::MutexGuard<'a, ReactorInner>,
        cond: &'a parking_lot::Condvar,
    }

    impl Drop for ReactorLock<'_> {
        fn drop(&mut self) {
            // Notify the reactor that the inner state has changed.
            self.cond.notify_one();
        }
    }

    impl ReactorLock<'_> {
        pub(crate) fn cancel_timer(&mut self, seq_num: u24, guid: u64, wakers: &mut Vec<Waker>) {
            let unique_seq = UniqueSeq { seq_num, guid };
            if let Some(when) = self.inner.mapping.remove(&unique_seq) {
                wakers.push(
                    self.inner
                        .timers
                        .remove(&(when, unique_seq))
                        .expect("timer should exist"),
                );
            }
        }
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
    use crate::tests::test_trace_log_setup;
    use crate::{Reliability, RoleContext};

    const TEST_RTO: Duration = Duration::from_millis(1200);

    #[test]
    fn test_resend_map_works() {
        let mut map = ResendMap::new(RoleContext::test_server());
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
                flags: Flags::new(Reliability::Unreliable, false),
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
                    flags: Flags::new(Reliability::Unreliable, false),
                    reliable_frame_index: None,
                    seq_frame_index: None,
                    ordered: None,
                    fragment: None,
                    body: Bytes::from_static(b"2"),
                },
                Frame {
                    flags: Flags::new(Reliability::Unreliable, false),
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
        let mut map = ResendMap::new(RoleContext::test_server());
        map.record(0.into(), vec![]);
        map.record(1.into(), vec![]);
        map.record(2.into(), vec![]);
        std::thread::sleep(TEST_RTO);
        map.record(3.into(), vec![]);
        let mut buffer = VecDeque::default();
        map.process_stales(&mut buffer);
        assert_eq!(map.map.len(), 1);
    }

    #[tokio::test]
    async fn test_resend_map_poll_wait() {
        let _guard = test_trace_log_setup();

        let mut map = ResendMap::new(RoleContext::test_server());
        map.record(0.into(), vec![]);
        std::thread::sleep(TEST_RTO);
        map.record(1.into(), vec![]);
        map.record(2.into(), vec![]);
        map.record(3.into(), vec![]);

        let mut buffer = VecDeque::default();

        let res = map.poll_wait(&mut Context::from_waker(Waker::noop()));
        assert!(matches!(res, Poll::Ready(_)));

        map.process_stales(&mut buffer);
        assert_eq!(map.map.len(), 3);

        std::future::poll_fn(|cx| map.poll_wait(cx)).await;
        map.process_stales(&mut buffer);
        assert!(map.map.len() < 3);
    }
}
