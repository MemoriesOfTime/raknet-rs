use core::fmt;

use super::zerocopy::U16;
use crate::define_inet_type;
use crate::inet::ecn::ExplicitCongestionNotification;
use crate::inet::ip;

//#    0                   1                   2                   3
//#    0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
//#   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//#   |Version|  IHL  |Type of Service|          Total Length         |
//#   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//#   |         Identification        |Flags|      Fragment Offset    |
//#   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//#   |  Time to Live |    Protocol   |         Header Checksum       |
//#   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//#   |                       Source Address                          |
//#   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//#   |                    Destination Address                        |
//#   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//#   |                    Options                    |    Padding    |
//#   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+

define_inet_type! {
    pub struct Header {
        vihl: Vihl,
        tos: Tos,
        total_len: U16,
        id: U16,
        flag_fragment: FlagFragment,
        ttl: u8,
        protocol: ip::Protocol,
        checksum: U16,
        source: IpV4Address,
        destination: IpV4Address,
    }
}

impl Header {
    /// Swaps the direction of the header
    #[inline]
    pub fn swap(&mut self) {
        core::mem::swap(&mut self.source, &mut self.destination)
    }

    #[inline]
    pub const fn vihl(&self) -> &Vihl {
        &self.vihl
    }

    #[inline]
    pub fn vihl_mut(&mut self) -> &mut Vihl {
        &mut self.vihl
    }

    #[inline]
    pub const fn tos(&self) -> &Tos {
        &self.tos
    }

    #[inline]
    pub fn tos_mut(&mut self) -> &mut Tos {
        &mut self.tos
    }

    #[inline]
    pub const fn total_len(&self) -> &U16 {
        &self.total_len
    }

    #[inline]
    pub fn total_len_mut(&mut self) -> &mut U16 {
        &mut self.total_len
    }

    #[inline]
    pub const fn id(&self) -> &U16 {
        &self.id
    }

    #[inline]
    pub fn id_mut(&mut self) -> &mut U16 {
        &mut self.id
    }

    #[inline]
    pub const fn flag_fragment(&self) -> &FlagFragment {
        &self.flag_fragment
    }

    #[inline]
    pub fn flag_fragment_mut(&mut self) -> &mut FlagFragment {
        &mut self.flag_fragment
    }

    #[inline]
    pub const fn ttl(&self) -> &u8 {
        &self.ttl
    }

    #[inline]
    pub fn ttl_mut(&mut self) -> &mut u8 {
        &mut self.ttl
    }

    #[inline]
    pub const fn protocol(&self) -> &ip::Protocol {
        &self.protocol
    }

    #[inline]
    pub fn protocol_mut(&mut self) -> &mut ip::Protocol {
        &mut self.protocol
    }

    #[inline]
    pub const fn checksum(&self) -> &U16 {
        &self.checksum
    }

    #[inline]
    pub fn checksum_mut(&mut self) -> &mut U16 {
        &mut self.checksum
    }

    #[inline]
    pub const fn source(&self) -> &IpV4Address {
        &self.source
    }

    #[inline]
    pub fn source_mut(&mut self) -> &mut IpV4Address {
        &mut self.source
    }

    #[inline]
    pub const fn destination(&self) -> &IpV4Address {
        &self.destination
    }

    #[inline]
    pub fn destination_mut(&mut self) -> &mut IpV4Address {
        &mut self.destination
    }

    #[inline]
    pub fn update_checksum(&mut self) {
        // TODO
    }
}

// the bits for version and IHL (header len).
define_inet_type! {
    pub struct Vihl {
        value: u8,
    }
}

impl fmt::Debug for Vihl {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Vihl")
            .field("version", &self.version())
            .field("header_len", &self.header_len())
            .finish()
    }
}

impl Vihl {
    #[inline]
    fn version(&self) -> u8 {
        self.value >> 4
    }

    #[inline]
    pub fn set_version(&mut self, value: u8) -> &mut Self {
        self.value = (value << 4) | (self.value & 0x0F);
        self
    }

    #[inline]
    pub fn header_len(&self) -> u8 {
        self.value & 0x0F
    }

    #[inline]
    pub fn set_header_len(&mut self, value: u8) -> &mut Self {
        self.value = (self.value & 0xF0) | (value & 0x0F);
        self
    }
}

define_inet_type! {
    pub struct Tos {
        value: u8,
    }
}

impl Tos {
    /// Differentiated Services Code Point
    #[inline]
    pub fn dscp(&self) -> u8 {
        self.value >> 2
    }

    #[inline]
    pub fn set_dscp(&mut self, value: u8) -> &mut Self {
        self.value = (value << 2) | (self.value & 0b11);
        self
    }

    #[inline]
    pub fn ecn(&self) -> ExplicitCongestionNotification {
        ExplicitCongestionNotification::new(self.value & 0b11)
    }

    #[inline]
    pub fn set_ecn(&mut self, ecn: ExplicitCongestionNotification) -> &mut Self {
        self.value = (self.value & !0b11) | ecn as u8;
        self
    }
}

impl fmt::Debug for Tos {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("ipv4::Tos")
            .field("dscp", &self.dscp())
            .field("ecn", &self.ecn())
            .finish()
    }
}

define_inet_type!(
    pub struct FlagFragment {
        value: U16,
    }
);

impl FlagFragment {
    const FRAGMENT_MASK: u16 = 0b0001_1111_1111_1111;

    #[inline]
    pub fn reserved(&self) -> bool {
        self.get(1 << 15)
    }

    pub fn set_reserved(&mut self, enabled: bool) -> &mut Self {
        self.set(1 << 15, enabled)
    }

    #[inline]
    pub fn dont_fragment(&self) -> bool {
        self.get(1 << 14)
    }

    #[inline]
    pub fn set_dont_fragment(&mut self, enabled: bool) -> &mut Self {
        self.set(1 << 14, enabled)
    }

    #[inline]
    pub fn more_fragments(&self) -> bool {
        self.get(1 << 13)
    }

    #[inline]
    pub fn set_more_fragments(&mut self, enabled: bool) -> &mut Self {
        self.set(1 << 13, enabled)
    }

    #[inline]
    pub fn fragment_offset(&self) -> u16 {
        self.value.get() & Self::FRAGMENT_MASK
    }

    #[inline]
    pub fn set_fragment_offset(&mut self, offset: u16) -> &mut Self {
        self.value
            .set(self.value.get() & !Self::FRAGMENT_MASK | offset & Self::FRAGMENT_MASK);
        self
    }

    #[inline]
    fn get(&self, mask: u16) -> bool {
        self.value.get() & mask == mask
    }

    #[inline]
    fn set(&mut self, mask: u16, enabled: bool) -> &mut Self {
        let value = self.value.get();
        let value = if enabled { value | mask } else { value & !mask };
        self.value.set(value);
        self
    }
}

impl fmt::Debug for FlagFragment {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("ipv4::FlagFragment")
            .field("reserved", &self.reserved())
            .field("dont_fragment", &self.dont_fragment())
            .field("more_fragments", &self.more_fragments())
            .field("fragment_offset", &self.fragment_offset())
            .finish()
    }
}

const IPV4_LEN: usize = 32 / 8;

define_inet_type!(
    pub struct IpV4Address {
        octets: [u8; IPV4_LEN],
    }
);
