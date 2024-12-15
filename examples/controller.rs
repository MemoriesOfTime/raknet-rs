#![feature(local_waker)]
#![feature(context_ext)]
#![allow(clippy::print_stdout)]

use std::error::Error;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::ContextBuilder;
use std::{cmp, io};

use bytes::Bytes;
use concurrent_queue::ConcurrentQueue;
use futures::future::poll_fn;
use futures::{Sink, SinkExt, StreamExt};
use raknet_rs::client::{self, ConnectTo};
use raknet_rs::opts::FlushStrategy;
use raknet_rs::server::MakeIncoming;
use raknet_rs::{server, Message};
use tokio::net::UdpSocket;

/// Self-balancing flush controller
struct FlushController {
    write: Pin<Box<dyn Sink<Message, Error = io::Error> + Send + Sync + 'static>>,
    next_flush: Option<tokio::time::Instant>,
    buffer: Arc<ConcurrentQueue<Message>>,
    delay: u64, // us
}

impl FlushController {
    fn new(write: Pin<Box<dyn Sink<Message, Error = io::Error> + Send + Sync + 'static>>) -> Self {
        Self {
            write,
            next_flush: None,
            buffer: Arc::new(ConcurrentQueue::unbounded()),
            delay: 5_000, // 5ms
        }
    }

    fn shared_sender(&self) -> Arc<ConcurrentQueue<Message>> {
        self.buffer.clone()
    }

    async fn wait(&self) {
        if let Some(next_flush) = self.next_flush {
            tokio::time::sleep_until(next_flush).await;
        }
    }

    async fn flush(&mut self) -> io::Result<()> {
        // Drain buffer
        while let Ok(msg) = self.buffer.pop() {
            self.write.feed(msg).await?;
        }

        // Flush
        let mut strategy = FlushStrategy::new(true, true, true);
        poll_fn(|cx| {
            let mut cx = ContextBuilder::from(cx).ext(&mut strategy).build();
            self.write.as_mut().poll_flush(&mut cx)
        })
        .await?;

        // A naive exponential backoff algorithm
        if strategy.flushed_ack() + strategy.flushed_nack() + strategy.flushed_pack() > 0 {
            self.delay = cmp::max(self.delay / 2, 5_000);
        } else {
            self.delay = cmp::min(self.delay * 2, 100_000);
        }
        self.next_flush =
            Some(tokio::time::Instant::now() + tokio::time::Duration::from_micros(self.delay));
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let local_addr = socket.local_addr()?;
    println!("[server] server listening on {local_addr} with flush controller");
    let mut incoming = socket.make_incoming(
        server::Config::new()
            .send_buf_cap(1024)
            .sever_guid(114514)
            .advertisement("Hello, I am proxy server")
            .min_mtu(500)
            .max_mtu(1400)
            .support_version(vec![9, 11, 13])
            .max_pending(64),
    );

    tokio::spawn(async move {
        loop {
            let (src, dst) = incoming.next().await.unwrap();
            tokio::spawn(async move {
                tokio::pin!(src);
                let mut controller = FlushController::new(Box::pin(dst));
                {
                    let sender1 = controller.shared_sender();
                    sender1
                        .push(Message::new(Bytes::from_static(b"Welcome to the server")))
                        .unwrap();
                }
                let sender2 = controller.shared_sender();
                loop {
                    tokio::select! {
                        Some(data) = src.next() => {
                            // echo back
                            sender2.push(Message::new(
                                data,
                            )).unwrap();
                        }
                        _ = controller.wait() => {
                            controller.flush().await.unwrap();
                        }
                    }
                }
            });
        }
    });
    client(local_addr).await?;
    Ok(())
}

async fn client(addr: SocketAddr) -> Result<(), Box<dyn Error>> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let (src, dst) = socket
        .connect_to(
            addr,
            client::Config::new()
                .send_buf_cap(1024)
                .mtu(1000)
                .client_guid(1919810)
                .protocol_version(11),
        )
        .await?;
    tokio::pin!(src);
    tokio::pin!(dst);
    dst.send(Message::new(Bytes::from_static(b"User pack1")))
        .await?;
    dst.send(Message::new(Bytes::from_static(b"User pack2")))
        .await?;
    let pack1 = src.next().await.unwrap();
    let pack2 = src.next().await.unwrap();
    let pack3 = src.next().await.unwrap();
    assert_eq!(pack1, Bytes::from_static(b"Welcome to the server"));
    assert_eq!(pack2, Bytes::from_static(b"User pack1"));
    assert_eq!(pack3, Bytes::from_static(b"User pack2"));
    Ok(())
}
