use core::cmp::Ordering;
use core::fmt;
use core::hash::{Hash, Hasher};

pub use zerocopy::*;

macro_rules! zerocopy_network_integer {
    ($native:ident, $name:ident) => {
        #[derive(
            Clone,
            Copy,
            Default,
            Eq,
            $crate::inet::zerocopy::FromBytes,
            $crate::inet::zerocopy::FromZeroes,
            $crate::inet::zerocopy::AsBytes,
            $crate::inet::zerocopy::Unaligned,
        )]
        #[repr(C)]
        pub struct $name(::zerocopy::byteorder::$name<NetworkEndian>);

        impl $name {
            pub const ZERO: Self = Self(::zerocopy::byteorder::$name::ZERO);

            #[inline(always)]
            pub fn new(value: $native) -> Self {
                value.into()
            }

            #[inline(always)]
            pub fn get(&self) -> $native {
                self.get_be().to_be()
            }

            #[inline(always)]
            pub fn get_be(&self) -> $native {
                unsafe {
                    $native::from_ne_bytes(
                        *(self.0.as_bytes().as_ptr()
                            as *const [u8; ::core::mem::size_of::<$native>()]),
                    )
                }
            }

            #[inline(always)]
            pub fn set(&mut self, value: $native) {
                self.0.as_bytes_mut().copy_from_slice(&value.to_be_bytes());
            }

            #[inline(always)]
            pub fn set_be(&mut self, value: $native) {
                self.0.as_bytes_mut().copy_from_slice(&value.to_ne_bytes());
            }
        }

        impl PartialEq for $name {
            #[inline]
            fn eq(&self, other: &Self) -> bool {
                self.cmp(other) == Ordering::Equal
            }
        }

        impl PartialEq<$native> for $name {
            #[inline]
            fn eq(&self, other: &$native) -> bool {
                self.partial_cmp(other) == Some(Ordering::Equal)
            }
        }

        impl PartialOrd for $name {
            #[inline]
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }

        impl PartialOrd<$native> for $name {
            #[inline]
            fn partial_cmp(&self, other: &$native) -> Option<Ordering> {
                Some(self.get().cmp(other))
            }
        }

        impl Ord for $name {
            #[inline]
            fn cmp(&self, other: &Self) -> Ordering {
                self.get_be().cmp(&other.get_be())
            }
        }

        impl Hash for $name {
            fn hash<H: Hasher>(&self, state: &mut H) {
                self.get().hash(state);
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                write!(formatter, "{}", self.get())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                write!(formatter, "{}", self.get())
            }
        }

        impl From<$native> for $name {
            #[inline]
            fn from(value: $native) -> Self {
                Self(::zerocopy::byteorder::$name::new(value))
            }
        }

        impl From<$name> for $native {
            #[inline]
            fn from(v: $name) -> $native {
                v.get()
            }
        }
    };
}

zerocopy_network_integer!(i16, I16);
zerocopy_network_integer!(u16, U16);
zerocopy_network_integer!(i32, I32);
zerocopy_network_integer!(u32, U32);
zerocopy_network_integer!(i64, I64);
zerocopy_network_integer!(u64, U64);
zerocopy_network_integer!(i128, I128);
zerocopy_network_integer!(u128, U128);
