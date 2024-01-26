/// Frame decoder
mod decoder;

/// Frame encoder
mod encoder;

use bytes::{Buf, BytesMut};
use derive_builder::Builder;
use futures::{Stream, StreamExt};
use tokio_util::codec::{Decoder, Encoder};
use tracing::{debug, trace};

use self::decoder::{DeFragmented, Deduplicated, FrameDecoded, Ordered};
use crate::errors::CodecError;
use crate::packet::connected::{FrameBody, FrameSet, Frames};
use crate::packet::Packet;
use crate::utils::Logged;

/// Codec config
#[derive(Clone, Copy, Debug, Builder)]
pub struct Config {
    /// Limit the max size of a parted frames set, 0 means no limit
    /// It will abort the split frame if the parted_size reaches limit.
    /// Enable it to avoid DoS attack.
    /// The maximum number of inflight parted frames is max_parted_size * max_parted_count
    max_parted_size: u32,
    /// Limit the max count of **all** parted frames sets from an address.
    /// It might cause client resending frames if the limit is reached.
    /// Enable it to avoid DoS attack.
    /// The maximum number of inflight parted frames is max_parted_size * max_parted_count
    max_parted_count: usize,
    /// Maximum ordered channel, the value should be less than 256
    max_channels: usize,
    // Limit the maximum deduplication gap for a connection, 0 means no limit.
    // Enable it to avoid D-DoS attack based on deduplication.
    max_dedup_gap: usize,
}

impl Default for Config {
    fn default() -> Self {
        // recommend configuration
        Self {
            max_parted_size: 256,
            max_parted_count: 256,
            max_channels: 1,
            max_dedup_gap: 1024,
        }
    }
}

pub(crate) trait Decoded {
    fn decoded(self, config: Config) -> impl Stream<Item = FrameBody>;
}

impl<F> Decoded for F
where
    F: Stream<Item = FrameSet<Frames<BytesMut>>>,
{
    fn decoded(self, config: Config) -> impl Stream<Item = FrameBody> {
        fn ok_f(pack: &FrameBody) {
            trace!("[decoder] received packet: {:?}", pack);
        }
        fn err_f(err: CodecError) {
            debug!("[decoder] got codec error: {err} when decode packet");
        }

        self.map(Ok)
            .deduplicated(config.max_dedup_gap)
            .defragmented(config.max_parted_size, config.max_parted_count)
            .ordered(config.max_channels)
            .frame_decoded()
            .logged_all(ok_f, err_f)
    }
}

/// The raknet codec
pub(crate) struct Codec;

impl<B: Buf> Encoder<Packet<Frames<B>>> for Codec {
    type Error = CodecError;

    fn encode(&mut self, item: Packet<Frames<B>>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        item.write(dst);
        Ok(())
    }
}

impl Decoder for Codec {
    type Error = CodecError;
    // we might want to update the package during codec
    type Item = Packet<Frames<BytesMut>>;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        Packet::read(src)
    }
}

/// Micro bench helper
#[cfg(feature = "micro-bench")]
pub mod micro_bench {
    use bytes::{BufMut, Bytes};
    use rand::rngs::StdRng;
    use rand::seq::SliceRandom;
    use rand::{Rng, SeedableRng};

    use super::{BytesMut, Config, Decoded, FrameSet, Frames, Stream};
    use crate::packet::connected::{Flags, Fragment, Frame, Ordered, Uint24le};
    use crate::packet::PackType;

    #[derive(derive_builder::Builder, Debug, Clone)]
    pub struct Options {
        config: Config,
        frame_set_cnt: usize,
        frame_per_set: usize,
        duplicated: bool,
        duplicated_ratio: f32,
        unordered: bool,
        parted: bool,
        parted_size: usize,
        shuffle: bool,
        seed: u64,
        data: Bytes,
    }

    impl Options {
        pub fn builder() -> OptionsBuilder {
            OptionsBuilder {
                config: None,
                frame_set_cnt: None,
                frame_per_set: None,
                duplicated: None,
                duplicated_ratio: None,
                unordered: None,
                parted: None,
                parted_size: None,
                shuffle: None,
                seed: None,
                data: None,
            }
        }

        fn gen_inputs(&self) -> Vec<FrameSet<Frames<BytesMut>>> {
            assert!(self.frame_per_set * self.frame_set_cnt % self.parted_size == 0);
            assert!(self.data.len() > self.parted_size);
            let mut rng = StdRng::seed_from_u64(self.seed);
            let frames: Frames<BytesMut> = std::iter::repeat(self.data.clone())
                .take(self.frame_per_set * self.frame_set_cnt)
                .enumerate()
                .map(|(idx, old_body)| {
                    let mut body = BytesMut::new();
                    // Game Packet
                    body.put_u8(PackType::Game as u8);
                    body.put(old_body);

                    let mut raw = 0;
                    let mut reliable_frame_index = None;
                    let mut fragment = None;
                    let mut ordered = None;
                    if self.duplicated {
                        raw |= 0b010 << 5;
                        reliable_frame_index = Some(Uint24le(idx as u32));
                    }
                    if self.parted {
                        raw |= 0b0001_0000;
                        let parted_start =
                            (idx % self.parted_size) * (body.len() / self.parted_size);
                        let parted_end = if idx % self.parted_size == self.parted_size - 1 {
                            body.len()
                        } else {
                            parted_start + (body.len() / self.parted_size)
                        };
                        let _ = body.split_to(parted_start);
                        let _ = body.split_off(parted_end - parted_start);
                        fragment = Some(Fragment {
                            parted_size: self.parted_size as u32,
                            parted_id: (idx / self.parted_size) as u16,
                            parted_index: (idx % self.parted_size) as u32,
                        });
                    }
                    if self.unordered {
                        raw |= 0b001 << 5;
                        ordered = Some(Ordered {
                            frame_index: Uint24le((idx / self.parted_size) as u32),
                            channel: 0,
                        });
                    }
                    Frame {
                        flags: Flags::parse(raw),
                        reliable_frame_index,
                        seq_frame_index: None,
                        ordered,
                        fragment,
                        body,
                    }
                })
                .flat_map(|frame| {
                    if rng.gen_ratio((self.duplicated_ratio * 100.0) as u32, 100) {
                        vec![frame.clone(), frame]
                    } else {
                        vec![frame]
                    }
                })
                .collect();
            let mut sets = frames
                .chunks(self.frame_per_set)
                .enumerate()
                .map(|(idx, chunk)| FrameSet {
                    seq_num: Uint24le(idx as u32),
                    set: chunk.to_vec(),
                })
                .collect::<Vec<_>>();
            if self.shuffle {
                sets.shuffle(&mut rng);
            }
            sets
        }

        pub fn input_data_pack_cnt(&self) -> usize {
            self.frame_per_set * self.frame_set_cnt / self.parted_size
        }

        pub fn input_data_pack_size(&self) -> usize {
            // omit the duplicated part
            self.data.len() * self.input_data_pack_cnt()
        }

        pub fn input_mtu(&self) -> usize {
            self.data.len() / self.parted_size
        }
    }

    #[derive(Debug)]
    pub struct MicroBench {
        config: Config,
        #[cfg(test)]
        data: Bytes,
        frame_sets: Vec<FrameSet<Frames<BytesMut>>>,
    }

    impl MicroBench {
        pub fn new(option: Options) -> Self {
            Self {
                config: option.config,
                #[cfg(test)]
                data: option.data.clone(),
                frame_sets: option.gen_inputs(),
            }
        }

        #[cfg(test)]
        #[allow(clippy::semicolon_if_nothing_returned)]
        async fn bench_decoded_checked(self) {
            use bytes::Buf as _;

            let config = self.config;
            let data = self.data.clone();
            let mut cnt = 0;
            let stream = self.into_stream().decoded(config);
            #[futures_async_stream::for_await]
            for res in stream {
                cnt += 1;
                let body = match res {
                    crate::packet::connected::FrameBody::Game(body) => body,
                    _ => unreachable!("unexpected decoded result"),
                };
                tracing::debug!("receive body: {body:?}, cnt: {cnt}");
                assert_eq!(body.chunk(), data.chunk());
            }
        }

        #[allow(clippy::semicolon_if_nothing_returned)]
        pub async fn bench_decoded(self) {
            let config = self.config;
            let stream = self.into_stream().decoded(config);
            #[futures_async_stream::for_await]
            for _r in stream {}
        }

        fn into_stream(mut self) -> impl Stream<Item = FrameSet<Frames<BytesMut>>> {
            #[futures_async_stream::stream]
            async move {
                while let Some(frame_set) = self.frame_sets.pop() {
                    yield frame_set;
                }
            }
        }
    }

    #[cfg(test)]
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn test_bench() {
        let opts = Options::builder()
            .config(Config::default())
            .frame_per_set(8)
            .frame_set_cnt(100)
            .duplicated(true)
            .duplicated_ratio(0.1)
            .unordered(true)
            .parted(true)
            .parted_size(4)
            .shuffle(true)
            .seed(114514)
            .data(Bytes::from_static(b"1145141919810"))
            .build()
            .unwrap();
        assert_eq!(
            opts.input_data_pack_size(),
            8 * 100 / 4 * "1145141919810".len()
        );
        let bench = MicroBench::new(opts);
        bench.bench_decoded_checked().await;
    }
}
