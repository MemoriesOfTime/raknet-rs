#![feature(local_waker)]
#![feature(context_ext)]
#![allow(clippy::print_stdout)]

use std::error::Error;
use std::pin::Pin;
use std::task::ContextBuilder;
use std::{cmp, io};

use futures::future::poll_fn;
use futures::{Sink, SinkExt, StreamExt};
use raknet_rs::opts::FlushStrategy;
use raknet_rs::server::MakeIncoming;
use raknet_rs::{server, Message};
use tokio::net::UdpSocket;

/// Self-balancing flush controller
struct FlushController {
    write: Pin<Box<dyn Sink<Message, Error = io::Error> + Send + Sync + 'static>>,
    next_flush: Option<tokio::time::Instant>,
    delay: u64, // us
}

impl FlushController {
    fn new(write: Pin<Box<dyn Sink<Message, Error = io::Error> + Send + Sync + 'static>>) -> Self {
        Self {
            write,
            next_flush: None,
            delay: 5_000, // 5ms
        }
    }

    async fn _flush0(&mut self) -> io::Result<()> {
        self.write.flush().await
    }

    async fn wait(&self) {
        if let Some(next_flush) = self.next_flush {
            tokio::time::sleep_until(next_flush).await;
        }
    }

    async fn flush(&mut self) -> io::Result<()> {
        let mut strategy = FlushStrategy::new(true, true, true);
        poll_fn(|cx| {
            let mut cx = ContextBuilder::from(cx).ext(&mut strategy).build();
            self.write.as_mut().poll_flush(&mut cx)
        })
        .await?;

        // Adjust delay
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
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let local_addr = socket.local_addr()?;
    println!("[server] server listening on {local_addr} with flush controller");
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
            let (src, dst) = incoming.next().await.unwrap();
            tokio::spawn(async move {
                tokio::pin!(src);
                let mut controller = FlushController::new(Box::pin(dst));
                loop {
                    tokio::select! {
                        Some(_data) = src.next() => {
                            // handle data
                        }
                        _ = controller.wait() => {
                            controller.flush().await.unwrap();
                        }
                    }
                }
            });
        }
    });
    Ok(())
}
