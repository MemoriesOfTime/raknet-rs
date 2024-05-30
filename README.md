![Header](https://capsule-render.vercel.app/api?type=Waving&color=timeGradient&height=300&animation=fadeIn&section=header&text=raknet-rs&fontSize=90&fontAlignY=45)

[![Crates.io](https://img.shields.io/crates/v/raknet-rs.svg?style=flat-square&logo=rust)](https://crates.io/crates/raknet-rs)
[![CI Status](https://img.shields.io/github/actions/workflow/status/MemoriesOfTime/raknet-rs/ci.yml?style=flat-square&logo=github)](https://github.com/MemoriesOfTime/raknet-rs/actions)
[![Coverage](https://img.shields.io/codecov/c/github/MemoriesOfTime/raknet-rs?style=flat-square&logo=codecov)](https://app.codecov.io/github/MemoriesOfTime/raknet-rs)
[![License](https://img.shields.io/crates/l/raknet-rs?style=flat-square)](https://github.com/MemoriesOfTime/raknet-rs/blob/master/LICENSE)

Yet another project rewritten in Rust.

## Features

- `Stream`/`Sink`/`Future` based async API.
  - Low level API but easy to use.
- RakNet features:
  - Support `Unreliable`, `Reliable` and `ReliableOrdered` packets.
  - Support multiple order channels.
  - Support `ACK`/`NACK` mechanism.
- Full tracing powered by [minitrace-rust](https://github.com/tikv/minitrace-rust).
  - You can track a packet's span during deduplication, fragmentation, ...

## Roadmap

> Ordered by priority

- Add sliding window congestion control
- Documentation
- More fuzz testing
- Bulk benchmark

## Getting Started

See [examples](examples/) for basic usage.

### Server

IO is a hidden type that implements the traits `Stream` and `Sink`.
Never stop polling `incoming` because it also serves as the router to every IOs.
Apply `Sink::poll_flush` to IO will trigger to flush all pending packets, `ACK`/`NACK`, and stale packets.

```rust
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use raknet_rs::server::{self, MakeIncoming};

let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await?;
let config = server::ConfigBuilder::default()
    .send_buf_cap(1024)
    .sever_guid(114514)
    .advertisement(Bytes::from_static(b"Hello, I am server"))
    ...
    .build()
    .unwrap();
let mut incoming = socket.make_incoming(config);
let mut io = incoming.next().await.unwrap();
let data: Bytes = io.next().await.unwrap();
io.send(data).await.unwrap();
```

### Client

> The current version of the client only has the most basic handshake implementation, and it is not recommended to use it directly.

```rust
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use raknet_rs::client::{self, ConnectTo};

let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
let config = client::ConfigBuilder::default()
    .send_buf_cap(1024)
    .client_guid(1919810)
    ...
    .build()
    .unwrap();
let mut conn = socket.connect_to(<addr>, config).await?;
conn.send(Bytes::from_static(b"Hello, Anyone there?"))
    .await?;
let res: Bytes = conn.next().await.unwrap();
```
