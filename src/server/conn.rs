use std::net::SocketAddr;

use pin_project_lite::pin_project;

pub(crate) enum Conn<F> {
    HandShaking(HandeShake<F>),
    Connected,
    Closed,
}

pin_project! {
    /// Process a raknet handshake, poll it to open a connection to the client
    pub(crate) struct HandeShake<F> {
        #[pin]
        pub(crate) frame: F,
        pub(crate) client_protocol_ver: u8,
        pub(crate) client_mtu: u16,
        pub(crate) peer_addr: SocketAddr,
    }
}
