#![allow(dead_code)]

use std::future::poll_fn;
use std::iter::repeat;
use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use futures_lite::StreamExt;
use log::{debug, info};
use tokio::net::UdpSocket;

use crate::client::{self, ConnectTo};
use crate::io::{Reader, Writer};
use crate::server::{self, MakeIncoming};
use crate::utils::tests::test_trace_log_setup;
use crate::{Message, Reliability};

fn make_server_conf() -> server::Config {
    server::Config::new()
        .send_buf_cap(1024)
        .sever_guid(1919810)
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
        .client_guid(114514)
        .protocol_version(11)
}

// Send data to the destination
async fn send(dst: Pin<&mut impl Writer<Message>>, data: impl Iterator<Item = u8>) {
    send_with_full(dst, data, Reliability::ReliableOrdered, 0).await;
}

async fn send_with_reliability(
    dst: Pin<&mut impl Writer<Message>>,
    data: impl Iterator<Item = u8>,
    reliability: Reliability,
) {
    send_with_full(dst, data, reliability, 0).await;
}

async fn send_with_order(
    dst: Pin<&mut impl Writer<Message>>,
    data: impl Iterator<Item = u8>,
    order: u8,
) {
    send_with_full(dst, data, Reliability::ReliableOrdered, order).await;
}

async fn send_with_full(
    mut dst: Pin<&mut impl Writer<Message>>,
    data: impl Iterator<Item = u8>,
    reliability: Reliability,
    order: u8,
) {
    poll_fn(|cx| dst.as_mut().poll_ready(cx)).await.unwrap();
    dst.as_mut()
        .feed(Message::new(reliability, order, Bytes::from_iter(data)));
    poll_fn(|cx| dst.as_mut().poll_flush(cx)).await.unwrap();
}

async fn flush(mut dst: Pin<&mut impl Writer<Message>>) {
    poll_fn(|cx| dst.as_mut().poll_flush(cx)).await.unwrap();
}

async fn close(mut dst: Pin<&mut impl Writer<Message>>) {
    poll_fn(|cx| dst.as_mut().poll_close(cx)).await.unwrap();
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
            let (src, dst) = incoming.next().await.unwrap();
            tokio::spawn(async move {
                tokio::pin!(src);
                tokio::pin!(dst);
                let mut ticker = tokio::time::interval(Duration::from_millis(5));
                loop {
                    tokio::select! {
                        Some(data) = src.next() => {
                            debug!("received data from {}", src.get_remote_addr());
                            send(dst.as_mut(), data.into_iter()).await;
                        }
                        _ = ticker.tick() => {
                            flush(dst.as_mut()).await;
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

        send(dst.as_mut(), repeat(0xfe).take(256)).await;
        assert_eq!(
            src.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(256))
        );
        send(dst.as_mut(), repeat(0xfe).take(512)).await;
        assert_eq!(
            src.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(512))
        );
        send(dst.as_mut(), repeat(0xfe).take(1024)).await;
        assert_eq!(
            src.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(1024))
        );
        send(dst.as_mut(), repeat(0xfe).take(2048)).await;
        assert_eq!(
            src.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(2048))
        );
        send(dst.as_mut(), repeat(0xfe).take(4096)).await;
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
                                send(dst.as_mut(), res.into_iter()).await;
                            } else {
                                break;
                            }
                        }
                        _ = ticker.tick() => {
                            // flush periodically to ensure all missing packets/ack are sent
                            flush(dst.as_mut()).await;
                        }
                    };
                }
                info!("connection closed by client, close the io");
                close(dst.as_mut()).await;
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
        send(dst.as_mut(), huge_msg.iter().copied()).await;
        assert_eq!(src.next().await.unwrap(), huge_msg);

        close(dst.as_mut()).await;

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
                    flush(dst.as_mut()).await;
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
