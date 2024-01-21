use std::net::SocketAddr;

use pin_project_lite::pin_project;

pub(crate) enum Conn<F> {
    HandShaking(HandShake<F>),
    Connected,
    Closed,
}

pin_project! {
    /// Process a raknet handshake, poll it to open a connection to the client
    pub(crate) struct HandShake<F> {
        #[pin]
        frame: F,
        client_protocol_ver: u8,
        client_mtu: u16,
        peer_addr: SocketAddr,
    }
}
