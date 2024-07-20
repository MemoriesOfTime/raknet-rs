use std::error::Error;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU8, Ordering};

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use raknet_rs::client::{self, ConnectTo};
use raknet_rs::io::IO;
use raknet_rs::server::{self, MakeIncoming};
use raknet_rs::Reliability;
use tokio::net::UdpSocket;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let local_addr = socket.local_addr()?;
    println!("[server] proxy server listening on {local_addr}");
    let mut incoming = socket.make_incoming(
        server::Config::new()
            .send_buf_cap(1024)
            .sever_guid(114514)
            .advertisement(&b"Hello, I am proxy server"[..])
            .min_mtu(500)
            .max_mtu(1400)
            .support_version(vec![9, 11, 13])
            .max_pending(64),
    );

    tokio::spawn(async move {
        loop {
            let io = incoming.next().await.unwrap();
            tokio::spawn(async move {
                static ORDER_CHANNEL: AtomicU8 = AtomicU8::new(0);

                tokio::pin!(io);
                println!("[server] set default reliability to Reliable");
                io.as_mut().set_default_reliability(Reliability::Reliable);
                loop {
                    if let Some(data) = io.next().await {
                        println!(
                            "[server] got proxy data: '{}'",
                            String::from_utf8_lossy(&data)
                        );
                        let client = reqwest::Client::new();
                        let res = client
                            .post("http://httpbin.org/post")
                            .body(data)
                            .send()
                            .await
                            .unwrap();
                        let channel = ORDER_CHANNEL.fetch_add(1, Ordering::Relaxed);
                        println!("[server] assign order channel: {}", channel);
                        io.as_mut().set_default_order_channel(channel);
                        io.send(res.bytes().await.unwrap()).await.unwrap();
                        continue;
                    }
                    break;
                }
            });
        }
    });

    client(local_addr, "paopao").await?;
    client(local_addr, "yui").await?;
    Ok(())
}

async fn client(addr: SocketAddr, name: &str) -> Result<(), Box<dyn Error>> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    println!("[{name}] I am listening on {}", socket.local_addr()?);
    let mut conn = socket
        .connect_to(
            addr,
            client::Config::new()
                .send_buf_cap(1024)
                .mtu(1000)
                .client_guid(1919810)
                .protocol_version(11),
        )
        .await?;
    conn.send(Bytes::from_static(b"Hello, Anyone there?"))
        .await?;
    let res = conn.next().await.unwrap();
    println!(
        "[{name}] got server response: {}",
        String::from_utf8_lossy(&res)
    );
    Ok(())
}
