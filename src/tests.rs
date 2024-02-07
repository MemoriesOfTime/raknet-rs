use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use minitrace::collector::{self, ConsoleReporter};
use minitrace::Event;
use tokio::net::UdpSocket;

use crate::client::{self, ConnectTo};
use crate::server::{self, MakeIncoming};

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        minitrace::flush();
    }
}

#[must_use]
fn test_setup() -> TestGuard {
    minitrace::set_reporter(ConsoleReporter, collector::Config::default());

    env_logger::Builder::from_default_env()
        .format(|_, record| {
            // Add a event to the current local span representing the log record
            Event::add_to_local_parent(record.level().as_str(), || {
                [("message".into(), record.args().to_string().into())]
            });
            // write nothing as we already use `ConsoleReporter`
            Ok(())
        })
        .filter_level(log::LevelFilter::Trace)
        .init();

    TestGuard
}

#[tokio::test]
async fn test_tokio_udp_works() {
    let _guard = test_setup();

    let server = async {
        let mut incoming = UdpSocket::bind("0.0.0.0:19132")
            .await
            .unwrap()
            .make_incoming(
                server::ConfigBuilder::default()
                    .send_buf_cap(1024)
                    .sever_guid(1919810)
                    .advertisement(Bytes::from_static(b"123456"))
                    .max_mtu(1500)
                    .min_mtu(510)
                    .max_pending(1024)
                    .support_version(vec![9, 11, 13])
                    .build()
                    .unwrap(),
            );
        loop {
            let mut io = incoming.next().await.unwrap();
            tokio::spawn(async move {
                loop {
                    let msg = io.next().await.unwrap();
                    assert_eq!(msg, Bytes::from_static(b"1919810"));
                    io.send(msg).await.unwrap();
                    io.send(Bytes::from_static(b"114514")).await.unwrap();
                }
            });
        }
    };

    tokio::spawn(server);

    let client = async {
        let mut io = UdpSocket::bind("0.0.0.0:0")
            .await
            .unwrap()
            .connect_to(
                "127.0.0.1:19132",
                client::ConfigBuilder::default()
                    .send_buf_cap(1024)
                    .mtu(1000)
                    .client_guid(114514)
                    .protocol_version(11)
                    .build()
                    .unwrap(),
            )
            .await
            .unwrap();

        io.send(Bytes::from_static(b"1919810")).await.unwrap();
        let msg = io.next().await.unwrap();
        assert_eq!(msg, Bytes::from_static(b"1919810"));
        let next_msg = io.next().await.unwrap();
        assert_eq!(next_msg, Bytes::from_static(b"114514"));
    };

    tokio::spawn(client).await.unwrap();
}
