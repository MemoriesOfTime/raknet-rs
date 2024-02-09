//! Micro benches

use std::time::Duration;

use bytes::BytesMut;
use criterion::async_executor::FuturesExecutor;
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use raknet_rs::micro_bench;

pub fn codec_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("codec");
    let seed = 114514;

    group.warm_up_time(Duration::from_secs(10));

    // large packets, every frame set only contains one frame
    {
        let opts = micro_bench::codec::Options::builder()
            .frame_per_set(1)
            .frame_set_cnt(14400)
            .duplicated_ratio(0.01)
            .unordered(true)
            .parted_size(4)
            .shuffle(false)
            .seed(seed)
            .data(BytesMut::from_iter(include_bytes!("data/body-large.txt")))
            .build()
            .unwrap();

        // total data size: 16369200 bytes, data count: 3600, mtu: 1136
        println!(
            "total data size: {} bytes, data count: {}, mtu: {}",
            opts.input_data_size(),
            opts.input_data_cnt(),
            opts.input_mtu(),
        );
        group.throughput(Throughput::Elements(opts.input_data_cnt() as u64));
        group.bench_function("decode_large_packets_same_data_cnt", |bencher| {
            bencher.to_async(FuturesExecutor).iter_batched(
                || micro_bench::codec::MicroBench::new(opts.clone()),
                |bench| bench.bench_decoded(),
                BatchSize::SmallInput,
            );
        });
    }

    // medium packets, every frame set contains 6 frame
    {
        let opts = micro_bench::codec::Options::builder()
            .frame_per_set(6)
            .frame_set_cnt(600)
            .duplicated_ratio(0.01)
            .unordered(true)
            .parted_size(1)
            .shuffle(false)
            .seed(seed)
            .data(BytesMut::from_iter(include_bytes!("data/body-medium.txt")))
            .build()
            .unwrap();

        // total data size: 630000 bytes, data count: 3600, mtu: 1050
        println!(
            "total data size: {} bytes, data count: {}, mtu: {}",
            opts.input_data_size(),
            opts.input_data_cnt(),
            opts.input_mtu(),
        );
        group.throughput(Throughput::Elements(opts.input_data_cnt() as u64));
        group.bench_function("decode_medium_packets_same_data_cnt", |bencher| {
            bencher.to_async(FuturesExecutor).iter_batched(
                || micro_bench::codec::MicroBench::new(opts.clone()),
                |bench| bench.bench_decoded(),
                BatchSize::SmallInput,
            );
        });
    }

    // short packets, every frame set contains 36 frame
    {
        let opts = micro_bench::codec::Options::builder()
            .frame_per_set(36)
            .frame_set_cnt(100)
            .duplicated_ratio(0.01)
            .unordered(true)
            .parted_size(1)
            .shuffle(false)
            .seed(seed)
            .data(BytesMut::from_iter(include_bytes!("data/body-short.txt")))
            .build()
            .unwrap();

        // total data size: 118800 bytes, data count: 3600, mtu: 1188
        println!(
            "total data size: {} bytes, data count: {}, mtu: {}",
            opts.input_data_size(),
            opts.input_data_cnt(),
            opts.input_mtu(),
        );
        group.throughput(Throughput::Elements(opts.input_data_cnt() as u64));
        group.bench_function("decode_short_packets_same_data_cnt", |bencher| {
            bencher.to_async(FuturesExecutor).iter_batched(
                || micro_bench::codec::MicroBench::new(opts.clone()),
                |bench| bench.bench_decoded(),
                BatchSize::SmallInput,
            );
        });
    }

    // large packets, every frame set only contains one frame
    {
        let opts = micro_bench::codec::Options::builder()
            .frame_per_set(1)
            .frame_set_cnt(1440)
            .duplicated_ratio(0.01)
            .unordered(true)
            .parted_size(4)
            .shuffle(false)
            .seed(seed)
            .data(BytesMut::from_iter(include_bytes!("data/body-large.txt")))
            .build()
            .unwrap();

        // total data size: 1,636,920 bytes, data count: 360, mtu: 1136
        println!(
            "total data size: {} bytes, data count: {}, mtu: {}",
            opts.input_data_size(),
            opts.input_data_cnt(),
            opts.input_mtu(),
        );
        group.throughput(Throughput::Bytes(opts.input_data_size() as u64));
        group.bench_function("decode_large_packets_same_data_size", |bencher| {
            bencher.to_async(FuturesExecutor).iter_batched(
                || micro_bench::codec::MicroBench::new(opts.clone()),
                |bench| bench.bench_decoded(),
                BatchSize::SmallInput,
            );
        });
    }

    // medium packets, every frame set contains 6 frame
    {
        let opts = micro_bench::codec::Options::builder()
            .frame_per_set(6)
            .frame_set_cnt(1550)
            .duplicated_ratio(0.01)
            .unordered(true)
            .parted_size(1)
            .shuffle(false)
            .seed(seed)
            .data(BytesMut::from_iter(include_bytes!("data/body-medium.txt")))
            .build()
            .unwrap();

        // total data size: 1,636,800 bytes, data count: 9300, mtu: 1056
        println!(
            "total data size: {} bytes, data count: {}, mtu: {}",
            opts.input_data_size(),
            opts.input_data_cnt(),
            opts.input_mtu(),
        );
        group.throughput(Throughput::Bytes(opts.input_data_size() as u64));
        group.bench_function("decode_medium_packets_same_data_size", |bencher| {
            bencher.to_async(FuturesExecutor).iter_batched(
                || micro_bench::codec::MicroBench::new(opts.clone()),
                |bench| bench.bench_decoded(),
                BatchSize::SmallInput,
            );
        });
    }

    // short packets, every frame set contains 36 frame
    {
        let opts = micro_bench::codec::Options::builder()
            .frame_per_set(36)
            .frame_set_cnt(1378)
            .duplicated_ratio(0.01)
            .unordered(true)
            .parted_size(1)
            .shuffle(false)
            .seed(seed)
            .data(BytesMut::from_iter(include_bytes!("data/body-short.txt")))
            .build()
            .unwrap();

        // total data size: 1,637,064 bytes, data count: 49608, mtu: 1188
        println!(
            "total data size: {} bytes, data count: {}, mtu: {}",
            opts.input_data_size(),
            opts.input_data_cnt(),
            opts.input_mtu(),
        );
        group.throughput(Throughput::Bytes(opts.input_data_size() as u64));
        group.bench_function("decode_short_packets_same_data_size", |bencher| {
            bencher.to_async(FuturesExecutor).iter_batched(
                || micro_bench::codec::MicroBench::new(opts.clone()),
                |bench| bench.bench_decoded(),
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

criterion_group!(benches, codec_benchmark);
criterion_main!(benches);
