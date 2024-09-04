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
- Full tracing:
  - You can track a packet's span during deduplication, fragmentation, ...

## Roadmap

> Ordered by priority

- Documentation
- Simulation testing
- Bulk benchmark
- Optimize performance for single thread runtime (IO without Send)
- Robust client implementation
- Use stable rust toolchain (~~I like nightly~~)

## Getting Started

See [examples](examples/) or [integration testing](src/tests.rs) for basic usage.

### Server

Most operations are performed on `Stream` and `Sink`. There will be some options in [opts](src/opts.rs).

Keep polling `incoming` because it also serves as the router to every connections.
Apply `Sink::poll_flush` to IO will trigger to flush all pending packets, `ACK`/`NACK`, and stale packets.
Apply `Sink::poll_close` to IO will ensure that all data is received by the peer before returning (i.e It may keep resending infinitely.).

> [!NOTE]
> All calculations are lazy. You need to decide how long to flush once, and how long to wait when closing before considering the peer is disconnected.

```rust
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use raknet_rs::server::{self, MakeIncoming};

let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await?;
let config = server::Config::new()
    .send_buf_cap(1024)
    .sever_guid(114514)
    .advertisement(&b"Hello, I am server"[..])
    ...
let mut incoming = socket.make_incoming(config);
let (reader, _) = incoming.next().await.unwrap();
tokio::pin!(reader);
let data: Bytes = reader.next().await.unwrap();
```

### Client

> [!WARNING]
> The current version of the client only has the most basic handshake implementation, and it is not recommended to use it directly.

```rust
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use raknet_rs::client::{self, ConnectTo};
use raknet_rs::Reliability;

let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
let config = client::Config::new()
    .send_buf_cap(1024)
    .client_guid(1919810)
    ...
let (_, writer) = socket.connect_to(<addr>, config).await?;
tokio::pin!(writer);
writer.send(Message::new(Reliability::Reliable, 0, Bytes::from_static(b"Hello, Anyone there?")))
    .await?;
```
