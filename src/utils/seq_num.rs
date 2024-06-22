use bytes::{Buf, BufMut};

/// Unsigned 24bits integer with litter endian (actually occupied 32 bits)
/// TODO: Can the sequence number wrap around?
#[allow(non_camel_case_types)]
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Default)]
pub(crate) struct u24(u32);

impl From<u24> for u32 {
    fn from(value: u24) -> Self {
        value.to_u32()
    }
}

impl From<u24> for usize {
    fn from(value: u24) -> Self {
        value.to_usize()
    }
}

impl From<u32> for u24 {
    fn from(value: u32) -> Self {
        debug_assert!((value >> 24) == 0, "{value} exceed the maximum of u24");
        Self(value)
    }
}

impl From<usize> for u24 {
    fn from(value: usize) -> Self {
        debug_assert!((value >> 24) == 0, "{value} exceed the maximum of u24");
        Self(value as u32)
    }
}

impl From<i32> for u24 {
    fn from(value: i32) -> Self {
        debug_assert!((value >> 24) == 0, "{value} exceed the maximum of u24");
        Self(value as u32)
    }
}

impl u24 {
    pub(crate) fn to_u32(self) -> u32 {
        self.0
    }

    pub(crate) fn to_usize(self) -> usize {
        self.0 as usize
    }
}

pub(crate) trait BufExt {
    fn get_u24_le(&mut self) -> u24;
}

pub(crate) trait BufMutExt {
    fn put_u24_le(&mut self, v: u24);
}

impl<B: Buf> BufExt for B {
    fn get_u24_le(&mut self) -> u24 {
        u24(self.get_uint_le(3) as u32)
    }
}

impl<B: BufMut> BufMutExt for B {
    fn put_u24_le(&mut self, v: u24) {
        self.put_uint_le(v.0 as u64, 3);
    }
}

impl core::fmt::Display for u24 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

///// Checked operations for u24

fn checked_add(a: u32, b: u32) -> u24 {
    let value = a + b;
    assert!((value >> 24) == 0, "{value} exceed the maximum of u24");
    u24(value)
}

impl core::ops::Add<&u24> for &u24 {
    type Output = u24;

    fn add(self, rhs: &u24) -> Self::Output {
        checked_add(self.0, rhs.0)
    }
}

impl core::ops::Add<&u24> for u24 {
    type Output = u24;

    fn add(self, rhs: &u24) -> Self::Output {
        checked_add(self.0, rhs.0)
    }
}

impl core::ops::Add<u24> for &u24 {
    type Output = u24;

    fn add(self, rhs: u24) -> Self::Output {
        checked_add(self.0, rhs.0)
    }
}

impl core::ops::Add<u24> for u24 {
    type Output = u24;

    fn add(self, rhs: u24) -> Self::Output {
        checked_add(self.0, rhs.0)
    }
}

impl core::ops::Add<u32> for u24 {
    type Output = u24;

    fn add(self, rhs: u32) -> Self::Output {
        checked_add(self.0, rhs)
    }
}

impl core::ops::Add<u32> for &u24 {
    type Output = u24;

    fn add(self, rhs: u32) -> Self::Output {
        checked_add(self.0, rhs)
    }
}

impl core::ops::AddAssign<&u24> for u24 {
    fn add_assign(&mut self, rhs: &u24) {
        self.0 += rhs.0;
        assert!((self.0 >> 24) == 0, "{self} exceed the maximum of u24");
    }
}

impl core::ops::AddAssign<u24> for u24 {
    fn add_assign(&mut self, rhs: u24) {
        self.0 += rhs.0;
        assert!((self.0 >> 24) == 0, "{self} exceed the maximum of u24");
    }
}

impl core::ops::AddAssign<u32> for u24 {
    fn add_assign(&mut self, rhs: u32) {
        self.0 += rhs;
        assert!((self.0 >> 24) == 0, "{self} exceed the maximum of u24");
    }
}

impl core::ops::Sub<&u24> for &u24 {
    type Output = u24;

    fn sub(self, rhs: &u24) -> Self::Output {
        u24(self.0 - rhs.0)
    }
}

impl core::ops::Sub<u24> for &u24 {
    type Output = u24;

    fn sub(self, rhs: u24) -> Self::Output {
        u24(self.0 - rhs.0)
    }
}

impl core::ops::Sub<&u24> for u24 {
    type Output = u24;

    fn sub(self, rhs: &u24) -> Self::Output {
        u24(self.0 - rhs.0)
    }
}

impl core::ops::Sub<u24> for u24 {
    type Output = u24;

    fn sub(self, rhs: u24) -> Self::Output {
        u24(self.0 - rhs.0)
    }
}

impl core::ops::Sub<u32> for &u24 {
    type Output = u24;

    fn sub(self, rhs: u32) -> Self::Output {
        u24(self.0 - rhs)
    }
}

impl core::ops::Sub<u32> for u24 {
    type Output = u24;

    fn sub(self, rhs: u32) -> Self::Output {
        u24(self.0 - rhs)
    }
}

impl core::ops::SubAssign<&u24> for u24 {
    fn sub_assign(&mut self, rhs: &u24) {
        self.0 -= rhs.0;
    }
}

impl core::ops::SubAssign<u24> for u24 {
    fn sub_assign(&mut self, rhs: u24) {
        self.0 -= rhs.0;
    }
}

impl core::ops::SubAssign<u32> for u24 {
    fn sub_assign(&mut self, rhs: u32) {
        self.0 -= rhs
    }
}
