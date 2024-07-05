#![allow(unused)]

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::DerefMut;
use std::sync::Arc;

use parking_lot::{Mutex, MutexGuard};

/// Create an unbounded priority channel. The smallest T with highest priority by default
pub(crate) fn unbounded<T: Ord>() -> (Sender<T>, Receiver<T>) {
    let share = Arc::new(PriorityChanShare {
        heap: Mutex::new(BinaryHeap::new()),
    });
    (Sender(Arc::clone(&share)), Receiver(share))
}

struct PriorityChanShare<T> {
    heap: Mutex<BinaryHeap<Reverse<T>>>,
}

/// Receive while holding the lock
struct BatchRecv<'a, T> {
    guard: MutexGuard<'a, BinaryHeap<Reverse<T>>>,
}

impl<'a, T: Ord> Iterator for BatchRecv<'a, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.guard.deref_mut().pop().map(|v| v.0)
    }
}

impl<T: Ord> PriorityChanShare<T> {
    fn send(&self, t: T) {
        self.heap.lock().push(Reverse(t));
    }

    fn send_batch(&self, t: impl IntoIterator<Item = T>) {
        self.heap.lock().extend(t.into_iter().map(Reverse));
    }

    fn recv(&self) -> Option<T> {
        self.heap.lock().pop().map(|v| v.0)
    }

    fn recv_batch(&self) -> BatchRecv<'_, T> {
        let guard = self.heap.lock();
        BatchRecv { guard }
    }

    fn is_empty(&self) -> bool {
        self.heap.lock().is_empty()
    }
}

pub(crate) struct Sender<T>(Arc<PriorityChanShare<T>>);

impl<T: Ord> Sender<T> {
    pub(crate) fn send(&self, t: T) {
        self.0.send(t);
    }

    pub(crate) fn send_batch(&self, t: impl IntoIterator<Item = T>) {
        self.0.send_batch(t);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

pub(crate) struct Receiver<T>(Arc<PriorityChanShare<T>>);

impl<T: Ord> Receiver<T> {
    pub(crate) fn recv(&self) -> Option<T> {
        self.0.recv()
    }

    /// Return a reception iterator that holds the lock. Please release it after obtaining what you
    /// need to prevent lock poisoning (check
    /// [`crate::utils::priority_mpsc::test`]`::test_lock_poisoning` ).
    pub(crate) fn recv_batch(&self) -> impl Iterator<Item = T> + '_ {
        self.0.recv_batch()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod test {
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use super::unbounded;

    #[test]
    fn test_it_works() {
        let (sender, receiver) = unbounded();
        let sender2 = sender.clone();
        sender.send(1);
        sender.send_batch(5..10);
        sender.send(2);
        sender2.send(3);
        assert_eq!(receiver.recv(), Some(1));
        let v = receiver.recv_batch().collect::<Vec<_>>();
        assert_eq!(v, vec![2, 3, 5, 6, 7, 8, 9]);
        assert!(sender.is_empty());
        assert!(receiver.is_empty());
    }

    #[test]
    fn test_reception_will_block_read() {
        let (sender, receiver) = unbounded();
        sender.send_batch(0..5);
        let mut reception = receiver.recv_batch();
        let handler = std::thread::spawn(move || {
            if !sender.is_empty() {
                panic!("check empty should be after reception");
            }
        });
        // ensure the thread starts
        std::thread::sleep(Duration::from_millis(50));
        let v = reception.take(5).collect::<Vec<_>>();
        assert_eq!(v, vec![0, 1, 2, 3, 4]);
        handler.join();
    }

    #[test]
    fn test_no_hidden_drop() {
        let (sender, receiver) = unbounded();
        sender.send_batch(0..5);
        let mut reception = receiver.recv_batch();
        let v1 = reception.take(2).collect::<Vec<_>>();
        assert_eq!(v1, vec![0, 1]);
        assert_eq!(receiver.recv(), Some(2));
        let v2 = receiver.recv_batch().collect::<Vec<_>>();
        assert_eq!(v2, vec![3, 4]);
    }

    // No poison support for now :(
    // https://github.com/Amanieu/parking_lot/issues/44
    //
    // > Generally the intended behavior on panic is to simply release any locks while unwinding.
    // 
    // #[test]
    // fn test_lock_poisoning() {
    //     let (sender, receiver) = unbounded();
    //     sender.send_batch(0..5);
    //     std::thread::spawn(move || {
    //         let mut reception = receiver.recv_batch();
    //         panic!("awsl");
    //     })
    //     .join();
    //     assert!(sender.0.heap.is_poisoned());
    // }

    #[test]
    fn test_lock_panic_release() {
        let (sender, receiver) = unbounded();
        sender.send_batch(0..5);
        std::thread::spawn(move || {
            let mut reception = receiver.recv_batch();
            panic!("awsl");
        })
        .join();
        // lock is released
        assert!(!sender.0.heap.is_locked());
    }
}
