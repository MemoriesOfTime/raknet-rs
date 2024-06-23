use std::iter::repeat;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use log::info;
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
    env_logger::init();
    TestGuard
}

#[tokio::test]
async fn test_tokio_udp_works() {
    let _guard = test_setup();

    let echo_server = async {
        let config = server::ConfigBuilder::default()
            .send_buf_cap(1024)
            .sever_guid(1919810)
            .advertisement(Bytes::from_static(b"123456"))
            .max_mtu(1500)
            .min_mtu(510)
            .max_pending(1024)
            .support_version(vec![9, 11, 13])
            .build()
            .unwrap();
        let mut incoming = UdpSocket::bind("0.0.0.0:19132")
            .await
            .unwrap()
            .make_incoming(config);
        loop {
            let mut io = incoming.next().await.unwrap();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(Duration::from_millis(10));
                loop {
                    tokio::select! {
                        res = io.next() => {
                            if let Some(data) = res {
                                io.send(data).await.unwrap();
                            } else {
                                info!("client closed connection");
                                break;
                            }
                        }
                        _ = ticker.tick() => {
                            SinkExt::<Bytes>::flush(&mut io).await.unwrap();
                        }
                    };
                }
            });
        }
    };

    tokio::spawn(echo_server);

    let client = async {
        let config = client::ConfigBuilder::default()
            .send_buf_cap(1024)
            .mtu(1000)
            .client_guid(114514)
            .protocol_version(11)
            .build()
            .unwrap();
        let mut io = UdpSocket::bind("0.0.0.0:0")
            .await
            .unwrap()
            .connect_to("127.0.0.1:19132", config)
            .await
            .unwrap();
        io.send(Bytes::from_iter(repeat(0xfe).take(2048)))
            .await
            .unwrap();
        assert_eq!(
            io.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(2048))
        );
        SinkExt::<Bytes>::close(&mut io).await.unwrap();
    };

    tokio::spawn(client).await.unwrap();
}
