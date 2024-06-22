use std::collections::VecDeque;

const DEFAULT_BIT_VEC_QUEUE_CAP: usize = 256;

/// A one-direction bit vector queue
/// It use a ring buffer `VecDeque<u128>` to store bits
#[derive(Debug, Clone)]
pub(crate) struct BitVecQueue {
    store: VecDeque<u128>,
    head: usize,
    tail: usize,
}

impl Default for BitVecQueue {
    #[inline]
    fn default() -> Self {
        Self::with_capacity(DEFAULT_BIT_VEC_QUEUE_CAP)
    }
}

impl BitVecQueue {
    pub(crate) fn with_capacity(cap: usize) -> Self {
        Self {
            store: VecDeque::with_capacity(cap),
            head: 0,
            tail: 0,
        }
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.tail - self.head
    }

    #[inline]
    fn to_physical_slot(&self, idx: usize) -> (usize, usize) {
        let physical_idx = idx + self.head;
        (physical_idx / 128, physical_idx & 127)
    }

    #[inline]
    pub(crate) fn get(&self, idx: usize) -> Option<bool> {
        if idx >= self.len() {
            return None;
        }
        let (idx, slot) = self.to_physical_slot(idx);
        Some(self.store[idx] & (1 << slot) != 0)
    }

    #[inline]
    pub(crate) fn set(&mut self, idx: usize, v: bool) {
        if idx >= self.len() {
            return;
        }
        let (idx, slot) = self.to_physical_slot(idx);
        let mask = 1 << slot;
        if v {
            self.store[idx] |= mask;
        } else {
            self.store[idx] &= !mask;
        }
    }

    pub(crate) fn push_back(&mut self, v: bool) {
        if let Some(back) = self.store.back_mut()
            && self.tail & 127 != 0
        {
            let mask = 1 << (self.tail & 127);
            if v {
                *back |= mask;
            } else {
                *back &= !mask;
            }
        } else {
            self.store.push_back(u128::from(v));
        }
        self.tail += 1;
    }

    pub(crate) fn front(&self) -> Option<bool> {
        self.store.front().map(|front| {
            let mask = 1 << self.head;
            front & mask != 0
        })
    }

    pub(crate) fn pop_front(&mut self) {
        if self.head == self.tail {
            self.clear();
            return;
        }
        self.head += 1;
        if self.head == 128 {
            self.store.pop_front();
            self.head = 0;
            self.tail -= 128;
        }
    }

    /// Clear the bit queue
    pub(crate) fn clear(&mut self) {
        self.head = 0;
        self.tail = 0;
        self.store.clear();
    }
}

#[cfg(test)]
mod test {
    use super::BitVecQueue;

    #[test]
    fn test_it_works() {
        let mut bit = BitVecQueue::default();
        assert_eq!(bit.front(), None);
        bit.push_back(true);
        bit.push_back(false);
        bit.push_back(true);
        assert_eq!(bit.front(), Some(true));
        bit.pop_front();
        assert_eq!(bit.get(0), Some(false));
        bit.set(1, false);
        assert_eq!(bit.get(1), Some(false));
        bit.pop_front();
        bit.pop_front();
        assert_eq!(bit.len(), 0);

        assert_eq!(bit.head, 3);
        assert_eq!(bit.tail, 3);
        assert!(!bit.store.is_empty());

        bit.pop_front();
        assert_eq!(bit.head, 0);
        assert_eq!(bit.tail, 0);
        assert!(bit.store.is_empty());
    }

    #[test]
    fn test_large_store() {
        let mut bit = BitVecQueue::default();
        for i in 0..1_000_000 {
            bit.push_back(i % 2 == 0);
        }
        for i in 0..1_000_000 {
            assert_eq!(bit.get(i), Some(i % 2 == 0));
        }
        for i in 0..1_000_000 {
            bit.set(i, i % 2 != 0);
        }
        for i in 0..1_000_000 {
            assert_eq!(bit.get(i), Some(i % 2 != 0));
        }
        assert_eq!(bit.len(), 1_000_000);
        for _ in 0..1_000_000 {
            bit.pop_front();
        }
        assert_eq!(bit.len(), 0);
        bit.set(2, true);
        assert_eq!(bit.get(2), None);
    }
}
