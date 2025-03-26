//# https://www.rfc-editor.org/rfc/rfc826

use crate::define_inet_type;

define_inet_type! {
    pub struct Header {
        destination: MacAddress,
        source: MacAddress,
        ethertype: EtherType,
    }
}

impl Header {
    /// Swaps the direction of the header
    #[inline]
    pub fn swap(&mut self) {
        core::mem::swap(&mut self.source, &mut self.destination);
    }

    #[inline]
    pub const fn destination(&self) -> &MacAddress {
        &self.destination
    }

    #[inline]
    pub fn destination_mut(&mut self) -> &mut MacAddress {
        &mut self.destination
    }

    #[inline]
    pub const fn source(&self) -> &MacAddress {
        &self.source
    }

    #[inline]
    pub fn source_mut(&mut self) -> &mut MacAddress {
        &mut self.source
    }

    #[inline]
    pub const fn ethertype(&self) -> &EtherType {
        &self.ethertype
    }

    #[inline]
    pub fn ethertype_mut(&mut self) -> &mut EtherType {
        &mut self.ethertype
    }
}

const MAC_LEN: usize = 48 / 8;

define_inet_type! {
    pub struct MacAddress {
        octets: [u8; MAC_LEN],
    }
}

define_inet_type! {
    pub struct EtherType {
        id: [u8; 2],
    }
}

macro_rules! impl_type {
    ($fun:ident, $cap:ident, $val:expr) => {
        pub const $cap: Self = Self { id: $val };

        #[inline]
        pub const fn $fun(self) -> bool {
            matches!(self, Self::$cap)
        }
    };
}

impl EtherType {
    impl_type!(is_ipv4, IPV4, [0x08, 0x00]);

    impl_type!(is_arp, ARP, [0x08, 0x06]);

    impl_type!(is_ipv6, IPV6, [0x86, 0xDD]);

    impl_type!(is_ppp, PPP, [0x88, 0x0B]);

    impl_type!(is_vlan, VLAN, [0x88, 0xA8]);
}
