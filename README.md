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
- Support message priority for unordered reliability.
- Full tracing:
  - You can track a packet's span during deduplication, fragmentation, ...

## Roadmap

> Ordered by priority

- Documentation
- Simulation testing
- Bulk benchmark
- AF_XDP socket & XDP redirect from UDP
- Optimize performance for single thread runtime (IO without Send)
- Robust client implementation
- Use stable rust toolchain (~~I like nightly~~)

## Getting Started

See [examples](examples/) or [integration testing](src/tests.rs) for basic usage.

### Server

Most operations are performed on `Stream` and `Sink`. There will be some options in [opts](src/opts.rs).

The implementation details are obscured, and you can only see a very high level of abstraction, including the `Error` type, which is just `std::io::Error`.

Keep polling `incoming` because it also serves as the router to every connections.

Apply `Sink::poll_flush` to IO will trigger to flush all pending packets, `ACK`/`NACK`, and stale packets. So you have to call `poll_flush` periodically. You can configure the [flush strategy](src/opts.rs) you want.

Apply `Sink::poll_close` to IO will ensure that all data is received by the peer before returning. It may keep resending infinitely unless you cancel the task. So you'd better set a timeout for each `poll_close`.

> [!NOTE]
> All calculations are lazy. The state will not update if you do not poll it.

```rust
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use raknet_rs::server::{self, MakeIncoming};

let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await?;
let config = server::Config::new()
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
    .client_guid(1919810)
    ...
let (_, writer) = socket.connect_to(<addr>, config).await?;
tokio::pin!(writer);
writer.send(Message::new(Bytes::from_static(b"Hello, Anyone there?")))
    .await?;
```
