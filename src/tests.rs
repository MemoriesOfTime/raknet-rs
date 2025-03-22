use std::future::poll_fn;
use std::iter::repeat;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::task::ContextBuilder;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use log::info;
use tokio::net::UdpSocket;

use crate::client::{self, ConnectTo};
use crate::opts::FlushStrategy;
use crate::server::{self, MakeIncoming};
use crate::utils::tests::{
    spawn_echo_server, test_trace_log_setup, RandomDataDisorder, RandomDataLost, RandomNetDelay,
    SimNet,
};
use crate::{Message, Priority, Reliability};

impl From<Bytes> for Message {
    fn from(data: Bytes) -> Self {
        Message::new(data)
    }
}

fn make_server_conf() -> server::Config {
    server::Config::new()
        .sever_guid(1919810)
        .max_channels(64)
        .advertisement("123456")
        .max_mtu(1500)
        .min_mtu(510)
        .max_pending(1024)
        .support_version(vec![9, 11, 13])
}

fn make_client_conf() -> client::Config {
    client::Config::new()
        .mtu(1000)
        .max_channels(64)
        .client_guid(114514)
        .protocol_version(11)
}

#[tokio::test(unhandled_panic = "shutdown_runtime")]
async fn test_tokio_udp_works() {
    let _guard = test_trace_log_setup();

    spawn_echo_server(
        UdpSocket::bind("0.0.0.0:19132")
            .await
            .unwrap()
            .make_incoming(make_server_conf()),
    );

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
async fn test_socket_works() {
    let _guard = test_trace_log_setup();
    let mut net = SimNet::bind(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
    let mut e1 = net.add_endpoint();
    let mut e2 = net.add_endpoint();
    net.connect(&mut e1, &mut e2);
    net.add_nemesis(&mut e1, &e2, RandomDataLost { odd: 0.02 });
    net.add_nemesis(
        &mut e1,
        &e2,
        RandomDataDisorder {
            odd: 0.02,
            rate: 0.2,
        },
    );
    net.add_nemesis(
        &mut e1,
        &e2,
        RandomNetDelay {
            odd: 0.02,
            delay: Duration::from_secs(1),
        },
    );
    let addr = e1.addr();

    spawn_echo_server(e1.make_incoming(make_server_conf()));

    let client = async move {
        let (src, dst) = e2.connect_to(addr, make_client_conf()).await.unwrap();

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
        dst.send(Bytes::from_iter(repeat(0xfe).take(8192)).into())
            .await
            .unwrap();
        assert_eq!(
            src.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(8192))
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
                                dst.feed(res.into()).await.unwrap();
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
        dst.send(huge_msg.clone().into()).await.unwrap();

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

#[tokio::test(unhandled_panic = "shutdown_runtime")]
async fn test_flush_strategy_works() {
    let _guard = test_trace_log_setup();

    let oneshot = async {
        let mut incoming = UdpSocket::bind("0.0.0.0:19134")
            .await
            .unwrap()
            .make_incoming(make_server_conf());
        loop {
            let (reader, sender) = incoming.next().await.unwrap();
            tokio::spawn(async move {
                tokio::pin!(reader);
                tokio::pin!(sender);
                let data = reader.next().await.unwrap();
                sender.feed(data.into()).await.unwrap();
                let mut strategy = FlushStrategy::new(false, false, true);
                poll_fn(|cx| {
                    let mut cx = ContextBuilder::from(cx).ext(&mut strategy).build();
                    // flush strategy only works in poll_flush
                    sender.poll_flush_unpin(&mut cx)
                })
                .await
                .unwrap();
                assert_eq!(strategy.flushed_pack(), 1); // flushed the packet feed before

                // not enabled, should panic
                std::panic::catch_unwind(|| strategy.flushed_ack()).unwrap_err();
                std::panic::catch_unwind(|| strategy.flushed_nack()).unwrap_err();
            });
        }
    };

    tokio::spawn(oneshot);

    let (src, dst) = UdpSocket::bind("0.0.0.0:0")
        .await
        .unwrap()
        .connect_to("127.0.0.1:19134", make_client_conf())
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
}

#[tokio::test(unhandled_panic = "shutdown_runtime")]
async fn test_message_priority_works() {
    let _guard = test_trace_log_setup();

    let recv = Arc::new(Mutex::new(Vec::new()));
    let recv_p = recv.clone();
    let server = async move {
        let mut incoming = UdpSocket::bind("0.0.0.0:19135")
            .await
            .unwrap()
            .make_incoming(make_server_conf());
        loop {
            let recv_c = recv_p.clone();
            let (reader, sender) = incoming.next().await.unwrap();
            tokio::spawn(async move {
                tokio::pin!(reader);
                tokio::pin!(sender);
                let mut ticker = tokio::time::interval(Duration::from_millis(5));
                loop {
                    tokio::select! {
                        Some(data) = reader.next() => {
                            recv_c.lock().unwrap().push(data);
                        }
                        _ = ticker.tick() => {
                            sender.flush().await.unwrap();
                        }
                    };
                }
            });
        }
    };

    tokio::spawn(server);

    let (_, dst) = UdpSocket::bind("0.0.0.0:0")
        .await
        .unwrap()
        .connect_to("127.0.0.1:19135", make_client_conf())
        .await
        .unwrap();
    tokio::pin!(dst);
    dst.feed(
        Message::new(Bytes::from_iter(repeat(0xfe).take(256))).reliability(Reliability::Reliable),
    )
    .await
    .unwrap();
    dst.feed(
        Message::new(Bytes::from_iter(repeat(0xfe).take(512)))
            .priority(Priority::High(0))
            .reliability(Reliability::Reliable),
    )
    .await
    .unwrap();

    dst.flush().await.unwrap();

    while recv.lock().unwrap().len() < 2 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(recv.lock().unwrap().len(), 2);
    assert_eq!(recv.lock().unwrap()[0].len(), 512);
    assert_eq!(recv.lock().unwrap()[1].len(), 256);
}

#[tokio::test(unhandled_panic = "shutdown_runtime")]
async fn test_tokio_udp_large_frame() {
    let _guard = test_trace_log_setup();

    spawn_echo_server(
        UdpSocket::bind("0.0.0.0:19136")
            .await
            .unwrap()
            .make_incoming(make_server_conf().max_parted_size(u32::MAX)),
    );

    let client = async {
        let (src, dst) = UdpSocket::bind("0.0.0.0:0")
            .await
            .unwrap()
            .connect_to(
                "127.0.0.1:19136",
                make_client_conf().max_parted_size(u32::MAX),
            )
            .await
            .unwrap();

        tokio::pin!(src);
        tokio::pin!(dst);

        dst.feed(Bytes::from_iter(repeat(0xfe).take(1 << 25)).into())
            .await
            .unwrap();
        let mut ticker = tokio::time::interval(Duration::from_millis(5));
        loop {
            tokio::select! {
                Some(data) = src.next() => {
                    assert_eq!(data.len(), 1 << 25);
                    break;
                }
                _ = ticker.tick() => {
                    // flush ack
                    dst.flush().await.unwrap();
                }
            }
        }
    };

    tokio::spawn(client).await.unwrap();
}
