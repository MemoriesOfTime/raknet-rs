use core::fmt;

use super::zerocopy::U16;
use crate::define_inet_type;

//# ------
//#                  0      7 8     15 16    23 24    31
//#                 +--------+--------+--------+--------+
//#                 |     Source      |   Destination   |
//#                 |      Port       |      Port       |
//#                 +--------+--------+--------+--------+
//#                 |                 |                 |
//#                 |     Length      |    Checksum     |
//#                 +--------+--------+--------+--------+
//#                 |
//#                 |          data octets ...
//#                 +---------------- ...

define_inet_type!(
    pub struct Header {
        source: U16,
        destination: U16,
        len: U16,
        checksum: U16,
    }
);

impl Header {
    /// Swaps the direction of the header
    #[inline]
    pub fn swap(&mut self) {
        core::mem::swap(&mut self.source, &mut self.destination);
    }

    #[inline]
    pub const fn source(&self) -> &U16 {
        &self.source
    }

    #[inline]
    pub fn source_mut(&mut self) -> &mut U16 {
        &mut self.source
    }

    #[inline]
    pub const fn destination(&self) -> &U16 {
        &self.destination
    }

    #[inline]
    pub fn destination_mut(&mut self) -> &mut U16 {
        &mut self.destination
    }

    #[inline]
    pub const fn len(&self) -> &U16 {
        &self.len
    }

    #[inline]
    pub fn len_mut(&mut self) -> &mut U16 {
        &mut self.len
    }

    #[inline]
    pub const fn checksum(&self) -> &U16 {
        &self.checksum
    }

    #[inline]
    pub fn checksum_mut(&mut self) -> &mut U16 {
        &mut self.checksum
    }
}

impl fmt::Debug for Header {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("udp::Header")
            .field("source", &self.source)
            .field("destination", &self.destination)
            .field("len", &self.len)
            .field("checksum", &format_args!("0x{:04x}", &self.checksum.get()))
            .finish()
    }
}
