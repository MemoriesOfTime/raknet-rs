use std::io;
use std::net::ToSocketAddrs;
use std::sync::Arc;

use bytes::Bytes;
use futures::{Sink, Stream};
use tokio::net::UdpSocket as TokioUdpSocket;

use super::ConnectTo;
use crate::opts::{ConnectionInfo, Ping};
use crate::Message;

impl ConnectTo for TokioUdpSocket {
    async fn connect_to(
        self,
        addrs: impl ToSocketAddrs,
        config: super::Config,
    ) -> io::Result<(
        impl Stream<Item = Bytes>,
        impl Sink<Message, Error = io::Error> + Ping + ConnectionInfo,
    )> {
        let socket = Arc::new(self);
        super::connect_to(socket, addrs, config, tokio::spawn).await
    }
}
