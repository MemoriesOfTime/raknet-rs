use bytes::BytesMut;
use criterion::async_executor::FuturesExecutor;
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use raknet_rs::codec;

pub fn codec_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("codec");
    let seed = 114514;

    {
        let opts = codec::micro_bench::Options::builder()
            .config(codec::Config::default())
            .frame_per_set(20)
            .frame_set_cnt(1000)
            .duplicated(true)
            .duplicated_ratio(0.1)
            .unordered(true)
            .parted(true)
            .parted_size(10)
            .shuffle(true)
            .seed(seed)
            .data(BytesMut::from_iter(include_bytes!("data/body-short.txt")))
            .build()
            .unwrap();

        println!("total data size: {} bytes", opts.input_data_pack_size());
        group.throughput(Throughput::Elements(opts.input_data_pack_cnt() as u64));
        group.bench_function(
            "decode(frame_per_set:20,frame_set_cnt:1000,duplicated,duplicated_ratio:0.1,unordered,parted,parted_size:10,shuffle,short_data)",
            |bencher| {
                bencher.to_async(FuturesExecutor).iter_batched(
                    || codec::micro_bench::MicroBench::new(opts.clone()),
                    |bench| bench.bench_decoded(),
                    BatchSize::SmallInput,
                );
            },
        );
    }

    {
        let opts = codec::micro_bench::Options::builder()
            .config(codec::Config::default())
            .frame_per_set(20)
            .frame_set_cnt(1000)
            .duplicated(true)
            .duplicated_ratio(0.1)
            .unordered(true)
            .parted(true)
            .parted_size(10)
            .shuffle(false)
            .seed(seed)
            .data(BytesMut::from_iter(include_bytes!("data/body-short.txt")))
            .build()
            .unwrap();

        println!("total data size: {} bytes", opts.input_data_pack_size());
        group.throughput(Throughput::Elements(opts.input_data_pack_cnt() as u64));
        group.bench_function(
            "decode(frame_per_set:20,frame_set_cnt:1000,duplicated,duplicated_ratio:0.1,unordered,parted,parted_size:10,no_shuffle,short_data)",
            |bencher| {
                bencher.to_async(FuturesExecutor).iter_batched(
                    || codec::micro_bench::MicroBench::new(opts.clone()),
                    |bench| bench.bench_decoded(),
                    BatchSize::SmallInput,
                );
            },
        );
    }

    {
        let opts = codec::micro_bench::Options::builder()
            .config(codec::Config::default())
            .frame_per_set(30)
            .frame_set_cnt(1000)
            .duplicated(true)
            .duplicated_ratio(0.1)
            .unordered(true)
            .parted(true)
            .parted_size(3)
            .shuffle(false)
            .seed(seed)
            .data(BytesMut::from_iter(include_bytes!("data/body-large.txt")))
            .build()
            .unwrap();

        // total data size: 45470000 bytes, data pack count: 10000, mtu: 1515
        println!(
            "total data size: {} bytes, data pack count: {}, mtu: {}",
            opts.input_data_pack_size(),
            opts.input_data_pack_cnt(),
            opts.input_mtu(),
        );
        group.throughput(Throughput::Bytes(opts.input_data_pack_size() as u64));
        group.bench_function(
            "decode(frame_per_set:30,frame_set_cnt:1000,duplicated,duplicated_ratio:0.1,unordered,parted,parted_size:3,no_shuffle,large_data)",
            |bencher| {
                bencher.to_async(FuturesExecutor).iter_batched(
                    || codec::micro_bench::MicroBench::new(opts.clone()),
                    |bench| bench.bench_decoded(),
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

criterion_group!(benches, codec_benchmark);
criterion_main!(benches);
