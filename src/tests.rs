#![allow(clippy::use_debug)]

use std::iter::repeat;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use log::info;
use tokio::net::UdpSocket;

use crate::client::{self, ConnectTo};
use crate::opts::TraceInfo;
use crate::server::{self, MakeIncoming};
use crate::utils::tests::test_trace_log_setup;
use crate::{Message, Reliability};

impl From<Bytes> for Message {
    fn from(data: Bytes) -> Self {
        Message::new(Reliability::ReliableOrdered, 0, data)
    }
}

fn make_server_conf() -> server::Config {
    server::Config::new()
        .send_buf_cap(1024)
        .sever_guid(1919810)
        .max_channels(64)
        .advertisement(&b"123456"[..])
        .max_mtu(1500)
        .min_mtu(510)
        .max_pending(1024)
        .support_version(vec![9, 11, 13])
}

fn make_client_conf() -> client::Config {
    client::Config::new()
        .send_buf_cap(1024)
        .mtu(1000)
        .max_channels(64)
        .client_guid(114514)
        .protocol_version(11)
}

#[tokio::test(unhandled_panic = "shutdown_runtime")]
async fn test_tokio_udp_works() {
    let _guard = test_trace_log_setup();

    let echo_server = async {
        let mut incoming = UdpSocket::bind("0.0.0.0:19132")
            .await
            .unwrap()
            .make_incoming(make_server_conf());
        loop {
            let (reader, sender) = incoming.next().await.unwrap();
            tokio::spawn(async move {
                tokio::pin!(reader);
                tokio::pin!(sender);
                let mut ticker = tokio::time::interval(Duration::from_millis(5));
                loop {
                    tokio::select! {
                        Some(data) = reader.next() => {
                            sender.feed(Message::new(Reliability::Reliable, 0, data)).await.unwrap();
                            info!("last trace id: {:?}", reader.last_trace_id());
                        }
                        _ = ticker.tick() => {
                            sender.flush().await.unwrap();
                        }
                    };
                }
            });
        }
    };

    tokio::spawn(echo_server);

    let client = async {
        let (src, dst) = UdpSocket::bind("0.0.0.0:0")
            .await
            .unwrap()
            .connect_to("127.0.0.1:19132", make_client_conf())
            .await
            .unwrap();

        tokio::pin!(src);
        tokio::pin!(dst);

        dst.send(Bytes::from_iter(repeat(0xfe).take(256)).into())
            .await
            .unwrap();
        assert_eq!(
            src.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(256))
        );
        dst.send(Bytes::from_iter(repeat(0xfe).take(512)).into())
            .await
            .unwrap();
        assert_eq!(
            src.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(512))
        );
        dst.send(Bytes::from_iter(repeat(0xfe).take(1024)).into())
            .await
            .unwrap();
        assert_eq!(
            src.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(1024))
        );
        dst.send(Bytes::from_iter(repeat(0xfe).take(2048)).into())
            .await
            .unwrap();
        assert_eq!(
            src.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(2048))
        );
        dst.send(Bytes::from_iter(repeat(0xfe).take(4096)).into())
            .await
            .unwrap();
        assert_eq!(
            src.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(4096))
        );
    };

    tokio::spawn(client).await.unwrap();
}

#[tokio::test(unhandled_panic = "shutdown_runtime")]
async fn test_4way_handshake_client_close() {
    let _guard = test_trace_log_setup();

    let server = async {
        let mut incoming = UdpSocket::bind("0.0.0.0:19133")
            .await
            .unwrap()
            .make_incoming(make_server_conf());
        loop {
            let (src, dst) = incoming.next().await.unwrap();
            tokio::spawn(async move {
                tokio::pin!(src);
                tokio::pin!(dst);
                let mut ticker = tokio::time::interval(Duration::from_millis(5));
                loop {
                    tokio::select! {
                        res = src.next() => {
                            if let Some(res) = res {
                                dst.feed(Message::new(Reliability::Unreliable, 0, res)).await.unwrap();
                            } else {
                                break;
                            }
                        }
                        _ = ticker.tick() => {
                            dst.flush().await.unwrap();
                        }
                    };
                }
                info!("connection closed by client, close the io");
                dst.close().await.unwrap();
                info!("io closed");
            });
        }
    };

    let client = async {
        let (src, dst) = UdpSocket::bind("0.0.0.0:0")
            .await
            .unwrap()
            .connect_to("127.0.0.1:19133", make_client_conf())
            .await
            .unwrap();

        tokio::pin!(src);
        tokio::pin!(dst);

        let huge_msg = Bytes::from_iter(repeat(0xfe).take(2048));
        dst.send(Message::new(
            Reliability::ReliableOrdered,
            0,
            huge_msg.clone(),
        ))
        .await
        .unwrap();

        assert_eq!(src.next().await.unwrap(), huge_msg);

        dst.close().await.unwrap();

        info!("client closed the connection, wait for server to close");

        let mut ticker = tokio::time::interval(Duration::from_millis(10));
        let mut last_2msl = false;
        let last_timer = tokio::time::sleep(Duration::from_millis(288));
        tokio::pin!(last_timer);
        loop {
            tokio::select! {
                None = src.next(), if !last_2msl => {
                    info!("received close notification from server, wait for 200ms(2MSL for test purpose)");
                    last_2msl = true;
                }
                _ = ticker.tick() => {
                    // flush periodically to ensure all missing packets/ack are sent
                    dst.flush().await.unwrap();
                }
                _ = &mut last_timer, if last_2msl => {
                    break;
                }
            };
        }
    };

    tokio::spawn(server);
    tokio::spawn(client).await.unwrap();
}
