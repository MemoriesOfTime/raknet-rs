//! Micro benches

use std::iter::repeat;

use bytes::Bytes;
use criterion::async_executor::FuturesExecutor;
use criterion::measurement::WallTime;
use criterion::{
    black_box, criterion_group, criterion_main, BatchSize, BenchmarkGroup, Criterion, Throughput,
};
use raknet_rs::micro_bench;
use raknet_rs::micro_bench::codec::BenchOpts;

pub fn codec_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("codec");

    fn decode(
        group: &mut BenchmarkGroup<WallTime>,
        datagram: &'static [u8],
        cnt: usize,
        throughput: impl Fn(&BenchOpts) -> Throughput,
    ) {
        let datagrams = repeat(Bytes::from_static(datagram)).take(cnt);
        let opts = micro_bench::codec::BenchOpts {
            datagrams: black_box(datagrams.collect()),
            seed: 114514,
            dup_ratio: 0.,
            shuffle_ratio: 0.,
            mtu: 1480,
        };
        group.throughput(throughput(&opts));
        group.bench_function(
            format!("decode_cnt-{cnt}_size-{}", datagram.len()),
            |bencher| {
                bencher.to_async(FuturesExecutor).iter_batched(
                    || opts.clone(),
                    |o| o.run_bench(),
                    BatchSize::SmallInput,
                );
            },
        );
    }

    let el = |opts: &BenchOpts| Throughput::Elements(opts.elements());
    let by = |opts: &BenchOpts| Throughput::Bytes(opts.bytes());

    let small_size = include_bytes!("data/small_size.txt");
    let medium_size = include_bytes!("data/medium_size.txt");
    let large_size = include_bytes!("data/large_size.txt");

    decode(&mut group, small_size, 1, el);
    decode(&mut group, small_size, 5, el);
    decode(&mut group, small_size, 10, el);
    decode(&mut group, small_size, 50, el);
    decode(&mut group, small_size, 100, el);
    decode(&mut group, small_size, 1000, el);

    decode(&mut group, small_size, 1, by);
    decode(&mut group, small_size, 5, by);
    decode(&mut group, small_size, 10, by);
    decode(&mut group, small_size, 50, by);
    decode(&mut group, small_size, 100, by);
    decode(&mut group, small_size, 1000, by);

    decode(&mut group, medium_size, 1, el);
    decode(&mut group, medium_size, 5, el);
    decode(&mut group, medium_size, 10, el);
    decode(&mut group, medium_size, 50, el);
    decode(&mut group, medium_size, 100, el);
    decode(&mut group, medium_size, 1000, el);

    decode(&mut group, medium_size, 1, by);
    decode(&mut group, medium_size, 5, by);
    decode(&mut group, medium_size, 10, by);
    decode(&mut group, medium_size, 50, by);
    decode(&mut group, medium_size, 100, by);
    decode(&mut group, medium_size, 1000, by);

    decode(&mut group, large_size, 1, el);
    decode(&mut group, large_size, 5, el);
    decode(&mut group, large_size, 10, el);
    decode(&mut group, large_size, 50, el);
    decode(&mut group, large_size, 100, el);
    decode(&mut group, large_size, 1000, el);

    decode(&mut group, large_size, 1, by);
    decode(&mut group, large_size, 5, by);
    decode(&mut group, large_size, 10, by);
    decode(&mut group, large_size, 50, by);
    decode(&mut group, large_size, 100, by);
    decode(&mut group, large_size, 1000, by);

    group.finish();
}

criterion_group!(benches, codec_benchmark);
criterion_main!(benches);
