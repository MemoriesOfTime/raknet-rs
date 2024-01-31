use futures::Stream;

use super::handler::offline;
use super::IO;
use crate::codec;

/// Incoming implementation from tokio's UDP framework
mod tokio;

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
    // use futures::{SinkExt, StreamExt};
    // use tokio::net::UdpSocket;
    // use tracing_test::traced_test;

    // use crate::server::handler::offline;
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
    //             offline: offline::Config {
    //                 sever_guid: 0,
    //                 advertisement: Bytes::from_static(b"MCPE"),
    //                 min_mtu: 500,
    //                 max_mtu: 1300,
    //                 support_version: vec![9, 11, 13],
    //                 max_pending: 512,
    //             },
    //         });
    //     loop {
    //         let io: IO = incoming.next().await.unwrap();
    //         tokio::spawn(async move {
    //             tokio::pin!(io);
    //             loop {
    //                 let msg: Bytes = io.next().await.unwrap();
    //                 println!("msg: {}", String::from_utf8_lossy(&msg));
    //                 io.send(msg).await.unwrap();
    //             }
    //         });
    //     }
    // }
}
