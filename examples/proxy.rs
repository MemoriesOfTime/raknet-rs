use std::error::Error;
use std::future::poll_fn;
use std::net::SocketAddr;

use bytes::Bytes;
use futures_lite::StreamExt;
use raknet_rs::client::{self, ConnectTo};
use raknet_rs::io::{Reader, Writer};
use raknet_rs::server::{self, MakeIncoming};
use raknet_rs::{Message, Reliability};
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
            let (reader, writer) = incoming.next().await.unwrap();
            tokio::spawn(async move {
                tokio::pin!(reader);
                tokio::pin!(writer);
                loop {
                    if let Some(data) = reader.next().await {
                        println!(
                            "[server] got proxy data: '{}' from {}",
                            String::from_utf8_lossy(&data),
                            reader.get_remote_addr()
                        );
                        let client = reqwest::Client::new();
                        let res = client
                            .post("http://httpbin.org/post")
                            .body(data)
                            .send()
                            .await
                            .unwrap();

                        poll_fn(|cx| writer.as_mut().poll_ready(cx)).await.unwrap();
                        writer.as_mut().feed(Message::new(
                            Reliability::Reliable,
                            0,
                            res.bytes().await.unwrap(),
                        ));
                        poll_fn(|cx| writer.as_mut().poll_flush(cx)).await.unwrap();

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
    let (reader, writer) = socket
        .connect_to(
            addr,
            client::Config::new()
                .send_buf_cap(1024)
                .mtu(1000)
                .client_guid(1919810)
                .protocol_version(11),
        )
        .await?;
    tokio::pin!(reader);
    tokio::pin!(writer);

    poll_fn(|cx| writer.as_mut().poll_ready(cx)).await.unwrap();
    writer.as_mut().feed(Message::new(
        Reliability::Reliable,
        0,
        Bytes::from_static(b"Hello, Anyone there?"),
    ));
    poll_fn(|cx| writer.as_mut().poll_flush(cx)).await.unwrap();

    let res = reader.next().await.unwrap();
    println!(
        "[{name}] got server response: {}",
        String::from_utf8_lossy(&res)
    );

    Ok(())
}
