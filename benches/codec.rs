// use bytes::Buf;
// use criterion::async_executor::FuturesExecutor;
// use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
// use raknet_rs::{micro_bench_codec_decode, micro_bench_codec_gen_data, CodecConfig};

// pub fn codec_benchmark(c: &mut Criterion) {
//     let mut group = c.benchmark_group("codec");

//     {
//         let data = micro_bench_codec_gen_data(
//             200_000,
//             200,
//             false,
//             false,
//             false,
//             0.0,
//             0.0,
//             &mut rand::thread_rng(),
//         );
//         group.throughput(Throughput::Bytes(
//             data.iter().map(|p| p.remaining() as u64).sum(),
//         ));
//         group.bench_function(
//             "decode(pack:200k,chunk:200,no shuffled,no dup)",
//             |bencher| {
//                 bencher.to_async(FuturesExecutor).iter_batched(
//                     || data.clone(),
//                     |data| micro_bench_codec_decode(data, CodecConfig::default()),
//                     BatchSize::SmallInput,
//                 );
//             },
//         );
//     }

//     {
//         let data = micro_bench_codec_gen_data(
//             200_000,
//             200,
//             true,
//             true,
//             true,
//             0.2,
//             0.2,
//             &mut rand::thread_rng(),
//         );
//         group.throughput(Throughput::Bytes(
//             data.iter().map(|p| p.remaining() as u64).sum(),
//         ));
//         group.bench_function(
//
// "decode(pack:200k,chunk:200,shuffled,frame_dup_threshold:0.2,chunk_dup_threshold:0.2)",
// |bencher| {                 bencher.to_async(FuturesExecutor).iter_batched(
//                     || data.clone(),
//                     |data| micro_bench_codec_decode(data, CodecConfig::default()),
//                     BatchSize::SmallInput,
//                 );
//             },
//         );
//     }

//     group.finish();
// }

// criterion_group!(benches, codec_benchmark);
// criterion_main!(benches);

fn main() {}
