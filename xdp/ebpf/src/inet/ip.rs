use crate::define_inet_type;

define_inet_type! {
    pub struct Protocol {
        id: u8,
    }
}

macro_rules! impl_p {
    ($fun:ident, $cap:ident, $val:literal) => {
        pub const $cap: Self = Self { id: $val };

        #[inline]
        pub const fn $fun(self) -> bool {
            self.id == $val
        }
    };
}

impl Protocol {
    // https://www.iana.org/assignments/protocol-numbers/protocol-numbers.xhtml
    // NOTE: these variants were added as the ones we think we'll need. feel free to add more as
    //       needed.
    impl_p!(is_hop_by_hop, HOPOPT, 0);

    impl_p!(is_icmp, ICMP, 1);

    impl_p!(is_ipv4, IPV4, 4);

    impl_p!(is_tcp, TCP, 6);

    impl_p!(is_udp, UDP, 17);

    impl_p!(is_ipv6, IPV6, 41);

    impl_p!(is_ipv6_route, IPV6_ROUTE, 43);

    impl_p!(is_ipv6_fragment, IPV6_FRAG, 44);

    impl_p!(is_ipv6_icmp, IPV6_ICMP, 58);

    impl_p!(is_ipv6_no_next, IPV6_NO_NXT, 59);

    impl_p!(is_ipv6_options, IPV6_OPTS, 60);

    impl_p!(is_udplite, UDPLITE, 136);
}
