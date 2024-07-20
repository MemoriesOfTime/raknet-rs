use std::collections::{HashMap, VecDeque};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use crate::packet::connected::{AckOrNack, Frame, Frames, Record};
use crate::utils::u24;

// TODO: use RTTEstimator to get adaptive RTO
const RTO: Duration = Duration::from_millis(144);

struct ResendEntry {
    frames: Option<Frames>,
    expired_at: Instant,
}

pub(crate) struct ResendMap {
    map: HashMap<u24, ResendEntry>,
    last_record_expired_at: Instant,
}

impl ResendMap {
    pub(crate) fn new() -> Self {
        Self {
            map: HashMap::new(),
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
        self.last_record_expired_at = min_expired_at;
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub(crate) fn size(&self) -> usize {
        self.map.len()
    }

    /// `poll_wait` suspends the task when the resend map needs to wait for the next resend
    pub(crate) fn poll_wait(&self, cx: &mut Context<'_>) -> Poll<()> {
        let expired_at;
        if let Some((_, entry)) = self.map.iter().min_by_key(|(_, entry)| entry.expired_at)
            && entry.expired_at > Instant::now()
        {
            expired_at = entry.expired_at;
        } else {
            return Poll::Ready(());
        }
        reactor::Reactor::get().insert_timer(expired_at, cx.waker());
        Poll::Pending
    }
}

/// Timer reactor
mod reactor {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::OnceLock;
    use std::task::Waker;
    use std::time::{Duration, Instant};
    use std::{mem, panic, thread};

    use log::trace;
    use parking_lot::{Condvar, Mutex};

    /// A simple timer reactor.
    ///
    /// There is only one global instance of this type, accessible by [`Reactor::get()`].
    pub(crate) struct Reactor {
        /// An ordered map of registered timers.
        ///
        /// Timers are in the order in which they fire. The `usize` in this type is a timer ID used
        /// to distinguish timers that fire at the same time. The `Waker` represents the
        /// task awaiting the timer.
        timers: Mutex<BTreeMap<(Instant, usize), Waker>>,
        cond: Condvar,
    }

    fn main_loop() {
        let reactor = Reactor::get();
        loop {
            reactor.process_timers();
        }
    }

    impl Reactor {
        pub(crate) fn get() -> &'static Reactor {
            static REACTOR: OnceLock<Reactor> = OnceLock::new();

            REACTOR.get_or_init(|| {
                // Spawn the daemon thread to motivate the reactor.
                thread::Builder::new()
                    .name("timer-reactor".to_string())
                    .spawn(main_loop)
                    .expect("cannot spawn timer-reactor thread");

                Reactor {
                    timers: Mutex::new(BTreeMap::new()),
                    cond: Condvar::new(),
                }
            })
        }

        /// Registers a timer in the reactor.
        ///
        /// Returns the inserted timer's ID.
        pub(crate) fn insert_timer(&self, when: Instant, waker: &Waker) -> usize {
            // Generate a new timer ID.
            static ID_GENERATOR: AtomicUsize = AtomicUsize::new(1);
            let id = ID_GENERATOR.fetch_add(1, Ordering::Relaxed);

            let mut guard = self.timers.lock();
            guard.insert((when, id), waker.clone());

            // Notify that a timer has been inserted.
            self.cond.notify_one();

            drop(guard);

            id
        }

        /// Processes ready timers and extends the list of wakers to wake.
        ///
        /// Returns the duration until the next timer before this method was called.
        fn process_timers(&self) {
            let mut timers = self.timers.lock();

            let now = Instant::now();

            // Split timers into ready and pending timers.
            //
            // Careful to split just *after* `now`, so that a timer set for exactly `now` is
            // considered ready.
            let pending = timers.split_off(&(now + Duration::from_nanos(1), 0));
            let ready = mem::replace(&mut *timers, pending);

            // Calculate the duration until the next event.
            let dur = timers
                .keys()
                .next()
                .map(|(when, _)| when.saturating_duration_since(now));

            for (_, waker) in ready {
                // TODO: wake up maybe slow down the reactor
                // Don't let a panicking waker blow everything up.
                panic::catch_unwind(|| waker.wake()).ok();
            }

            if let Some(dur) = dur {
                trace!("[timer_reactor] wait for {dur:?}");
                self.cond.wait_for(&mut timers, dur);
            } else {
                trace!("[timer_reactor] wait for next timer insertion");
                self.cond.wait(&mut timers);
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
    use crate::Reliability;

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
        const TEST_RTO: Duration = Duration::from_millis(100);

        let mut map = ResendMap::new();
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

        map.process_stales(&mut buffer);
        assert_eq!(map.map.len(), 3);

        std::future::poll_fn(|cx| map.poll_wait(cx)).await;
        map.process_stales(&mut buffer);
        assert!(map.map.len() < 3);
    }
}
