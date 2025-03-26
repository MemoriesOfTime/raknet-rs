pub mod ecn;
pub mod ethernet;
pub mod ip;
pub mod ipv4;
pub mod ipv6;
pub mod udp;

pub mod zerocopy;

#[macro_export]
macro_rules! define_inet_type {
    ($($vis:ident)? struct $name:ident {
        $(
            $field:ident: $field_ty:ty
        ),*
        $(,)*
    }) => {
        #[derive(Clone, Copy, Default, Eq, PartialEq, PartialOrd, Ord, zerocopy::FromZeroes, zerocopy::FromBytes, zerocopy::AsBytes, zerocopy::Unaligned)]
        #[repr(C)]
        $($vis)? struct $name {
            $(
                pub(crate) $field: $field_ty,
            )*
        }

        // By letting the compiler derive PartialEq, we can do structural matching on these
        // structs. But we also want hashing to be on the byte level, since we know the struct
        // has no allocations and has a simple layout.
        //
        // See: https://godbolt.org/z/czohnrWxK
        #[allow(clippy::derived_hash_with_manual_eq)]
        impl core::hash::Hash for $name {
            #[inline]
            fn hash<H: core::hash::Hasher>(&self, hasher: &mut H) {
                self.as_bytes().hash(hasher);
            }
        }

        impl $name {
            #[allow(non_camel_case_types)]
            #[allow(clippy::too_many_arguments)]
            #[inline]
            pub fn new<$($field: Into<$field_ty>),*>($($field: $field),*) -> Self {
                Self {
                    $(
                        $field: $field.into()
                    ),*
                }
            }

            #[inline]
            pub fn as_bytes(&self) -> &[u8] {
                zerocopy::AsBytes::as_bytes(self)
            }

            #[inline]
            pub fn as_bytes_mut(&mut self) -> &mut [u8] {
                zerocopy::AsBytes::as_bytes_mut(self)
            }

            #[inline]
            pub fn decode(buffer: $crate::buffer::DecodeBuffer<'_>) -> Result<(&Self, $crate::buffer::DecodeBuffer<'_>), $crate::buffer::DecoderError> {
                let (value, buffer) = buffer.decode_slice(core::mem::size_of::<$name>())?;
                let value = value.into_bytes();
                let value = unsafe {
                    // Safety: the type implements FromBytes
                    &*(value as *const _ as *const $name)
                };
                Ok((value, buffer))
            }
        }
    };
}
