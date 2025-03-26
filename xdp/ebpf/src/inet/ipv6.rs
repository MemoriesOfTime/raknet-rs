use core::fmt;

use super::ecn::ExplicitCongestionNotification;
use super::zerocopy::U16;
use crate::define_inet_type;
use crate::inet::ip;

//= https://www.rfc-editor.org/rfc/rfc8200#section-3
//#   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//#   |Version| Traffic Class |           Flow Label                  |
//#   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//#   |         Payload Length        |  Next Header  |   Hop Limit   |
//#   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//#   |                                                               |
//#   + +
//#   |                                                               |
//#   + Source Address                        +
//#   |                                                               |
//#   + +
//#   |                                                               |
//#   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//#   |                                                               |
//#   + +
//#   |                                                               |
//#   + Destination Address                      +
//#   |                                                               |
//#   + +
//#   |                                                               |
//#   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+

define_inet_type! {
    pub struct Header {
        vtcfl: Vtcfl,
        payload_len: U16,
        next_header: ip::Protocol,
        hop_limit: u8,
        source: IpV6Address,
        destination: IpV6Address,
    }
}

impl Header {
    /// Swaps the direction of the header
    #[inline]
    pub fn swap(&mut self) {
        core::mem::swap(&mut self.source, &mut self.destination);
    }

    #[inline]
    pub const fn vtcfl(&self) -> &Vtcfl {
        &self.vtcfl
    }

    #[inline]
    pub fn vtcfl_mut(&mut self) -> &mut Vtcfl {
        &mut self.vtcfl
    }

    #[inline]
    pub const fn payload_len(&self) -> &U16 {
        &self.payload_len
    }

    #[inline]
    pub fn payload_len_mut(&mut self) -> &mut U16 {
        &mut self.payload_len
    }

    #[inline]
    pub const fn next_header(&self) -> &ip::Protocol {
        &self.next_header
    }

    #[inline]
    pub fn next_header_mut(&mut self) -> &mut ip::Protocol {
        &mut self.next_header
    }

    #[inline]
    pub const fn hop_limit(&self) -> &u8 {
        &self.hop_limit
    }

    #[inline]
    pub fn hop_limit_mut(&mut self) -> &mut u8 {
        &mut self.hop_limit
    }

    #[inline]
    pub const fn source(&self) -> &IpV6Address {
        &self.source
    }

    #[inline]
    pub fn source_mut(&mut self) -> &mut IpV6Address {
        &mut self.source
    }

    #[inline]
    pub const fn destination(&self) -> &IpV6Address {
        &self.destination
    }

    #[inline]
    pub fn destination_mut(&mut self) -> &mut IpV6Address {
        &mut self.destination
    }
}

define_inet_type! {
    pub struct Vtcfl {
        octets: [u8; 4],
    }
}

impl fmt::Debug for Vtcfl {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("ipv6::Vtf")
            .field("version", &self.version())
            .field("dscp", &self.dscp())
            .field("ecn", &self.ecn())
            .field("flow_label", &format_args!("0x{:05x}", self.flow_label()))
            .finish()
    }
}

impl Vtcfl {
    #[inline]
    pub const fn version(&self) -> u8 {
        self.octets[0] >> 4
    }

    #[inline]
    pub fn set_version(&mut self, version: u8) -> &mut Self {
        self.octets[0] = (version << 4) | self.octets[0] & 0x0F;
        self
    }

    #[inline]
    pub fn dscp(&self) -> u8 {
        let value = (self.octets[0] << 4) | (self.octets[1] >> 4);
        value >> 2
    }

    #[inline]
    pub fn set_dscp(&mut self, value: u8) -> &mut Self {
        let value = value << 2;
        self.octets[0] = self.octets[0] & 0xF0 | (value >> 4);
        self.octets[1] = (value << 4) | self.octets[1] & 0b11_1111;
        self
    }

    #[inline]
    pub fn ecn(&self) -> ExplicitCongestionNotification {
        ExplicitCongestionNotification::new((self.octets[1] >> 4) & 0b11)
    }

    #[inline]
    pub fn set_ecn(&mut self, ecn: ExplicitCongestionNotification) -> &mut Self {
        self.octets[1] = (self.octets[1] & !(0b11 << 4)) | ((ecn as u8) << 4);
        self
    }

    #[inline]
    pub const fn flow_label(&self) -> u32 {
        u32::from_be_bytes([0, self.octets[1] & 0x0F, self.octets[2], self.octets[3]])
    }

    #[inline]
    pub fn set_flow_label(&mut self, flow_label: u32) -> &mut Self {
        let bytes = flow_label.to_be_bytes();
        self.octets[1] = self.octets[1] & 0xF0 | bytes[1] & 0x0F;
        self.octets[2] = bytes[2];
        self.octets[3] = bytes[3];
        self
    }
}

const IPV6_LEN: usize = 128 / 8;

define_inet_type!(
    pub struct IpV6Address {
        octets: [u8; IPV6_LEN],
    }
);
