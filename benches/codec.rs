use bytes::BytesMut;
use criterion::async_executor::FuturesExecutor;
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use raknet_rs::codec;

pub fn codec_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("codec");
    let seed = 114514;

    // large packets, every frame set only contains one frame
    {
        let opts = codec::micro_bench::Options::builder()
            .config(codec::Config::default())
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
        group.bench_function("decode_large_packets", |bencher| {
            bencher.to_async(FuturesExecutor).iter_batched(
                || codec::micro_bench::MicroBench::new(opts.clone()),
                |bench| bench.bench_decoded(),
                BatchSize::SmallInput,
            );
        });
    }

    // medium packets, every frame set only contains 10 frame
    {
        let opts = codec::micro_bench::Options::builder()
            .config(codec::Config::default())
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
        group.bench_function("decode_medium_packets", |bencher| {
            bencher.to_async(FuturesExecutor).iter_batched(
                || codec::micro_bench::MicroBench::new(opts.clone()),
                |bench| bench.bench_decoded(),
                BatchSize::SmallInput,
            );
        });
    }

    // short packets, every frame set only contains 30 frame
    {
        let opts = codec::micro_bench::Options::builder()
            .config(codec::Config::default())
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
        group.bench_function("decode_short_packets", |bencher| {
            bencher.to_async(FuturesExecutor).iter_batched(
                || codec::micro_bench::MicroBench::new(opts.clone()),
                |bench| bench.bench_decoded(),
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

criterion_group!(benches, codec_benchmark);
criterion_main!(benches);
