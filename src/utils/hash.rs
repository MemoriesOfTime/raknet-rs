use std::hash::{BuildHasher, Hasher};

// Only support integer key with maximum 64 bits width.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NoHashBuilder;

impl BuildHasher for NoHashBuilder {
    type Hasher = NoHashHasher;

    fn build_hasher(&self) -> Self::Hasher {
        NoHashHasher(0)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct NoHashHasher(u64);

impl Hasher for NoHashHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write_u8(&mut self, i: u8) {
        self.0 = u64::from(i);
    }

    fn write_u16(&mut self, i: u16) {
        self.0 = u64::from(i);
    }

    fn write_u32(&mut self, i: u32) {
        self.0 = u64::from(i);
    }

    fn write_u64(&mut self, i: u64) {
        self.0 = i;
    }

    fn write_usize(&mut self, i: usize) {
        self.0 = i as u64;
    }

    fn write_i8(&mut self, i: i8) {
        self.0 = i as u64;
    }

    fn write_i16(&mut self, i: i16) {
        self.0 = i as u64;
    }

    fn write_i32(&mut self, i: i32) {
        self.0 = i as u64;
    }

    fn write_i64(&mut self, i: i64) {
        self.0 = i as u64;
    }

    fn write_isize(&mut self, i: isize) {
        self.0 = i as u64;
    }

    fn write_u128(&mut self, _: u128) {
        unimplemented!("unsupported")
    }

    fn write_i128(&mut self, _: i128) {
        unimplemented!("unsupported")
    }

    fn write(&mut self, _: &[u8]) {
        unimplemented!("unsupported")
    }
}

// From Google's city hash.
#[inline(always)]
pub(crate) fn combine_hashes(upper: u64, lower: u64) -> u64 {
    const MUL: u64 = 0x9ddfea08eb382d69;

    let mut a = (lower ^ upper).wrapping_mul(MUL);
    a ^= a >> 47;
    let mut b = (upper ^ a).wrapping_mul(MUL);
    b ^= b >> 47;
    b = b.wrapping_mul(MUL);
    b
}
