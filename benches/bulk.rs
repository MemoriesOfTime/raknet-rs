use std::net::SocketAddr;
use std::sync::OnceLock;

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BatchSize, Bencher, Criterion, Throughput};
use futures::{SinkExt, StreamExt};
use raknet_rs::client::{self, ConnectTo};
use raknet_rs::server::{self, MakeIncoming};
use tokio::net::UdpSocket as TokioUdpSocket;
use tokio::runtime::Runtime;

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
        group.throughput(Throughput::Bytes(short_data.len() as u64 * 1000));
        group.bench_function("short_data_1000_clients", |bencher| {
            configure_bencher(bencher, server_addr, short_data, 1000)
        });
    }

    {
        group.throughput(Throughput::Bytes(medium_data.len() as u64));
        group.bench_function("medium_data_1_client", |bencher| {
            configure_bencher(bencher, server_addr, medium_data, 1)
        });
    }

    {
        group.throughput(Throughput::Bytes(medium_data.len() as u64 * 100));
        group.bench_function("medium_data_100_clients", |bencher| {
            configure_bencher(bencher, server_addr, medium_data, 100)
        });
    }

    {
        group.throughput(Throughput::Bytes(large_data.len() as u64));
        group.bench_function("large_data_1_client", |bencher| {
            configure_bencher(bencher, server_addr, large_data, 1)
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
    clients: usize,
) {
    let mk_client = || async move {
        let sock = TokioUdpSocket::bind("0.0.0.0:0").await.unwrap();
        sock.connect_to(
            server_addr,
            client::ConfigBuilder::default()
                .send_buf_cap(1024)
                .mtu(1400)
                .client_guid(1919810)
                .protocol_version(11)
                .build()
                .unwrap(),
        )
        .await
        .unwrap()
    };
    bencher.to_async(rt()).iter_batched(
        || std::iter::repeat_with(mk_client).take(clients),
        |handshakes| async move {
            let mut join = vec![];
            for handshake in handshakes {
                let handle = tokio::spawn(async move {
                    let mut client = handshake.await;
                    client.send(Bytes::from_static(data)).await.unwrap();
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
        let config = server::ConfigBuilder::default()
            .send_buf_cap(1024)
            .sever_guid(114514)
            .advertisement(Bytes::from_static(b"Hello, I am proxy server"))
            .min_mtu(500)
            .max_mtu(1400)
            .support_version(vec![9, 11, 13])
            .max_pending(1024)
            .build()
            .unwrap();
        let mut incoming = sock.make_incoming(config);
        while let Some(mut io) = incoming.next().await {
            tokio::spawn(async move { while io.next().await.is_some() {} });
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
