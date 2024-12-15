use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::task::Waker;
use std::time::{Duration, Instant};
use std::{mem, panic, thread};

use super::NoHashBuilder;
use crate::HashMap;

/// Timers are in the order in which they fire. The `usize` in this type is a timer ID used to
/// distinguish timers that fire at the same time. The `Waker` represents the task awaiting
/// the timer.
type Timers = BTreeMap<(Instant, usize), Waker>;

/// A reactor that manages timers.
pub(crate) struct Reactor {
    /// Map of registered timers, distinguished by their unique timer key.
    conn_timers: parking_lot::Mutex<HashMap<u64, Timers, NoHashBuilder>>,
    /// A condvar used to wake up the reactor when timers changed.
    cond: parking_lot::Condvar,
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
                .name("raknet-timer-reactor".to_string())
                .spawn(main_loop)
                .expect("cannot spawn timer-reactor thread");

            Reactor {
                conn_timers: parking_lot::Mutex::new(HashMap::default()),
                cond: parking_lot::Condvar::new(),
            }
        })
    }

    pub(crate) fn insert_timer(&self, key: u64, when: Instant, waker: &Waker) {
        let mut timers = self.conn_timers.lock();
        let timers = timers.entry(key).or_default();
        timers.insert((when, timers.len()), waker.clone());
        self.cond.notify_one();
    }

    pub(crate) fn cancel_all_timers(&self, key: u64) -> impl Iterator<Item = Waker> {
        let mut timers = self.conn_timers.lock();
        let res = timers
            .remove(&key)
            .into_iter()
            .flat_map(BTreeMap::into_values);
        self.cond.notify_one();
        res
    }

    /// Processes ready timers and waits for the next timer changed.
    fn process_timers(&self) {
        let mut region_timers = self.conn_timers.lock();
        let now = Instant::now();

        let mut dur: Option<Duration> = None;

        for timers in region_timers.values_mut() {
            // Split timers into ready and pending timers.
            //
            // Careful to split just *after* `now`, so that a timer set for exactly `now` is
            // considered ready.
            let pending = timers.split_off(&(now + Duration::from_nanos(1), 0));
            let ready = mem::replace(timers, pending);
            let next_time = timers
                .keys()
                .next()
                .map(|&(when, _)| when.saturating_duration_since(now));
            for (_, waker) in ready {
                // Don't let a panicking waker blow everything up.
                panic::catch_unwind(|| waker.wake()).ok();
            }
            match (dur, next_time) {
                (Some(d), Some(n)) => dur = Some(d.min(n)),
                (None, Some(n)) => dur = Some(n),
                _ => {}
            }
        }

        if let Some(dur) = dur {
            self.cond.wait_for(&mut region_timers, dur);
        } else {
            self.cond.wait(&mut region_timers);
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::utils::tests::TestWaker;

    #[test]
    fn test_it_works() {
        let reactor = Reactor::get();

        let dur = Duration::from_millis(100);
        let when = Instant::now() + dur;
        {
            let (waker, test) = TestWaker::pair();
            reactor.insert_timer(1, when, &waker);
            assert_eq!(reactor.cancel_all_timers(1).count(), 1);
            assert!(!test.woken.load(std::sync::atomic::Ordering::Relaxed));
        }

        {
            let (waker, test) = TestWaker::pair();
            reactor.insert_timer(2, when, &waker);
            std::thread::sleep(dur + Duration::from_millis(10));
            assert_eq!(reactor.cancel_all_timers(2).count(), 0);
            assert!(test.woken.load(std::sync::atomic::Ordering::Relaxed));
        }
    }
}
