use bytes::{Buf, BufMut};

/// Unsigned 24bits integer with litter endian (actually occupied 32 bits)
/// TODO: Can the sequence number wrap around?
#[allow(non_camel_case_types)]
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Default)]
pub(crate) struct u24(u32);

impl u24 {
    pub(crate) fn to_u32(self) -> u32 {
        self.0
    }

    pub(crate) fn to_usize(self) -> usize {
        self.0 as usize
    }
}

macro_rules! for_all_primitives {
    ($macro:ident) => {
        $macro! { u8, u16, u32, u64, usize, i8, i16, i32, i64, isize }
    };
}

macro_rules! impl_to_for_u24 {
    ($($t:ty),*) => {
        $(
            impl From<u24> for $t {
                fn from(value: u24) -> Self {
                    value.0 as $t
                }
            }
        )*
    };
}

for_all_primitives! { impl_to_for_u24 }

macro_rules! impl_from_for_u24 {
    ($($t:ty),*) => {
        $(
            impl From<$t> for u24 {
                fn from(value: $t) -> Self {
                    debug_assert!((value as u32 >> 24) == 0, "{value} exceed the maximum of u24");
                    Self(value as u32)
                }
            }
        )*
    };
}

for_all_primitives! { impl_from_for_u24 }

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

// for primitives

macro_rules! impl_add_for_u24 {
    ($($t:ty),*) => {
        $(
            impl core::ops::Add<$t> for u24 {
                type Output = u24;

                fn add(self, rhs: $t) -> Self::Output {
                    checked_add(self.0, rhs as u32)
                }
            }

            impl core::ops::Add<$t> for &u24 {
                type Output = u24;

                fn add(self, rhs: $t) -> Self::Output {
                    checked_add(self.0, rhs as u32)
                }
            }
        )*
    };
}

for_all_primitives! { impl_add_for_u24 }

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

// for primitives

macro_rules! impl_add_assign_for_u24 {
    ($($t:ty),*) => {
        $(
            impl core::ops::AddAssign<$t> for u24 {
                fn add_assign(&mut self, rhs: $t) {
                    self.0 += rhs as u32;
                    assert!((self.0 >> 24) == 0, "{self} exceed the maximum of u24");
                }
            }
        )*
    };
}

for_all_primitives! { impl_add_assign_for_u24 }

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

// for primitives

macro_rules! impl_sub_for_u24 {
    ($($t:ty),*) => {
        $(
            impl core::ops::Sub<$t> for &u24 {
                type Output = u24;

                fn sub(self, rhs: $t) -> Self::Output {
                    u24(self.0 - rhs as u32)
                }
            }

            impl core::ops::Sub<$t> for u24 {
                type Output = u24;

                fn sub(self, rhs: $t) -> Self::Output {
                    u24(self.0 - rhs as u32)
                }
            }
        )*
    };
}

for_all_primitives! { impl_sub_for_u24 }

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

macro_rules! impl_sub_assign_for_u24 {
    ($($t:ty),*) => {
        $(
            impl core::ops::SubAssign<$t> for u24 {
                fn sub_assign(&mut self, rhs: $t) {
                    self.0 -= rhs as u32;
                }
            }
        )*
    };
}

for_all_primitives! { impl_sub_assign_for_u24 }

// almost no multiplication, division or other operations on u24

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    #[should_panic]
    fn test_u24_overflow_1() {
        let mut a1: u24 = 0.into();
        a1 -= 1;
    }

    #[test]
    #[should_panic]
    fn test_u24_overflow_2() {
        let _a1: u24 = (1 << 24).into();
    }

    #[test]
    #[should_panic]
    fn test_u24_overflow_3() {
        let mut a1: u24 = ((1 << 24) - 1).into();
        a1 += 1;
    }

    #[test]
    #[should_panic]
    fn test_u24_overflow_4() {
        let a1: u24 = ((1 << 24) - 1).into();
        let _b1 = a1 + 1;
    }

    #[test]
    #[should_panic]
    fn test_u24_overflow_5() {
        let a1: u24 = 0.into();
        let _b1 = a1 - 1;
    }

    #[test]
    fn test_u24_works() {
        let a1: u24 = 1.into();
        let mut a2: u24 = 2.into();
        a2 += 1;
        let mut b1 = a1 + a2;
        b1 -= 1;
        assert_eq!(b1.to_u32(), 3);
    }
}
