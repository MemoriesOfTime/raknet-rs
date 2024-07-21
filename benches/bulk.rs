use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::Duration;

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BatchSize, Bencher, Criterion, Throughput};
use futures::{SinkExt, StreamExt};
use log::debug;
use raknet_rs::client::{self, ConnectTo};
use raknet_rs::server::{self, MakeIncoming};
use tokio::net::UdpSocket as TokioUdpSocket;
use tokio::runtime::Runtime;

const SEND_BUF_CAP: usize = 1024;
const MTU: u16 = 1500;

pub fn bulk_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("bulk_benchmark");
    let server_addr = spawn_server();

    env_logger::init();

    log::info!("server listening on {server_addr}");

    let large_data = include_bytes!("data/body-large.txt");
    let medium_data = include_bytes!("data/body-medium.txt");
    let short_data = include_bytes!("data/body-short.txt");

    {
        group.throughput(Throughput::Bytes(short_data.len() as u64));
        group.bench_function("short_data_1_client", |bencher| {
            configure_bencher(bencher, server_addr, short_data, 1)
        });
    }

    {
        group.throughput(Throughput::Bytes(medium_data.len() as u64));
        group.bench_function("medium_data_1_client", |bencher| {
            configure_bencher(bencher, server_addr, medium_data, 1)
        });
    }

    {
        group.throughput(Throughput::Bytes(large_data.len() as u64));
        group.bench_function("large_data_1_client", |bencher| {
            configure_bencher(bencher, server_addr, large_data, 1)
        });
    }

    // The following benchmarks are not stable, and the reason is as follows:
    // Some ack may fail to wake up a certain client (This client has already received the ack
    // before falling asleep to wait RTO, causing the ack not to wake it up. This is almost
    // impossible to happen in real-life scenarios.), and this client will wait for a complete
    // RTO period before receiving this ack. This ultimately causes this round of benchmarking to
    // stall for a while, affecting the benchmark results.

    // TODO: find a way to make the benchmark stable
    {
        group.throughput(Throughput::Bytes(short_data.len() as u64 * 10));
        group.bench_function("short_data_10_clients", |bencher| {
            configure_bencher(bencher, server_addr, short_data, 10)
        });
    }

    {
        group.throughput(Throughput::Bytes(medium_data.len() as u64 * 10));
        group.bench_function("medium_data_10_clients", |bencher| {
            configure_bencher(bencher, server_addr, medium_data, 10)
        });
    }

    {
        group.throughput(Throughput::Bytes(large_data.len() as u64 * 10));
        group.bench_function("large_data_10_clients", |bencher| {
            configure_bencher(bencher, server_addr, large_data, 10)
        });
    }

    group.finish();
}

fn configure_bencher(
    bencher: &mut Bencher<'_>,
    server_addr: SocketAddr,
    data: &'static [u8],
    clients_num: usize,
) {
    let mk_client = || async move {
        let sock = TokioUdpSocket::bind("0.0.0.0:0").await.unwrap();
        sock.connect_to(
            server_addr,
            client::Config::default()
                .send_buf_cap(SEND_BUF_CAP)
                .mtu(MTU)
                .protocol_version(11),
        )
        .await
        .unwrap()
    };
    bencher.to_async(rt()).iter_batched(
        || {
            std::iter::repeat_with(mk_client)
                .take(clients_num)
                .map(futures::executor::block_on)
        },
        |clients| async move {
            let mut join = vec![];
            for (i, client) in clients.enumerate() {
                let handle = tokio::spawn(async move {
                    tokio::pin!(client);
                    client.feed(Bytes::from_static(data)).await.unwrap();
                    debug!("client {} finished feeding", i);
                    client.close().await.unwrap(); // make sure all data is sent
                    debug!("client {} closed", i);
                });
                join.push(handle);
            }
            for handle in join {
                handle.await.unwrap();
            }
        },
        BatchSize::SmallInput,
    );
}

fn spawn_server() -> SocketAddr {
    let sock = rt().block_on(async { TokioUdpSocket::bind("127.0.0.1:0").await.unwrap() });
    let server_addr = sock.local_addr().unwrap();
    rt().spawn(async move {
        let config = server::Config::new()
            .send_buf_cap(SEND_BUF_CAP)
            .advertisement(&b"Hello, I am server"[..])
            .min_mtu(500)
            .max_mtu(MTU)
            .support_version(vec![9, 11, 13])
            .max_pending(1024);
        let mut incoming = sock.make_incoming(config);
        while let Some(io) = incoming.next().await {
            tokio::spawn(async move {
                tokio::pin!(io);
                // 20ms, one tick, from Minecraft
                let mut ticker = tokio::time::interval(Duration::from_millis(20));
                loop {
                    tokio::select! {
                        None = io.next() => {
                            break;
                        }
                        _ = ticker.tick() => {
                            assert!(io.flush().await.is_ok());
                        }
                    };
                }
                // No 2MSL for benchmark, so just wait for a while
                // On the server side, it may never receive the final acknowledgment of FIN
                // (Usually, the peer that initiates the closure first will wait for
                // 2MSL to ensure.). It is necessary to ensure that when the server
                // closes, all previously sent data packets are sent. Therefore, the
                // client will receive an acknowledgment of the FIN it sends and can
                // proceed with a normal closure.
                let _ = tokio::time::timeout(Duration::from_millis(100), io.close()).await;
            });
        }
    });
    server_addr
}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn rt() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("bulk-bench")
            .build()
            .unwrap()
    })
}

criterion_group!(benches, bulk_benchmark);
criterion_main!(benches);
