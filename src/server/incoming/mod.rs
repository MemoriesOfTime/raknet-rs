use futures::Stream;

use super::IO;
use crate::codec;
use crate::server::offline;

/// Incoming implementation from tokio's UDP framework
mod tokio;

/// Implementation of [`server::IO`]
mod io;

/// Incoming config
#[derive(Debug, Clone, derive_builder::Builder)]
pub struct Config {
    /// The send buffer of each IO polled by the incoming
    send_buf_cap: usize,
    codec: codec::Config,
    offline: offline::Config,
}

pub trait MakeIncoming: Sized {
    fn make_incoming(self, config: Config) -> impl Stream<Item = IO>;
}

#[cfg(test)]
mod test {

    // use bytes::Bytes;
    // use futures::StreamExt;
    // use tokio::net::UdpSocket;
    // use tracing_test::traced_test;

    // use crate::server::incoming::{Config, MakeIncoming};
    // use crate::server::IO;

    // #[traced_test]
    // #[tokio::test]
    // async fn test_tokio_incoming_works() {
    //     let mut incoming = UdpSocket::bind("0.0.0.0:19132")
    //         .await
    //         .unwrap()
    //         .make_incoming(Config {
    //             send_buf_cap: 1024,
    //             codec: Default::default(),
    //             offline: crate::server::offline::Config {
    //                 sever_guid: 114514,
    //                 advertisement: Bytes::from_static(b"hello"),
    //                 min_mtu: 800,
    //                 max_mtu: 1300,
    //                 support_version: vec![9, 11, 13],
    //                 max_pending: 512,
    //             },
    //         });
    //     let io: IO = incoming.next().await.unwrap();
    // }
}
