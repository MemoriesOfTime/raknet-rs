/// Frame decoder
mod decoder;

/// Frame encoder
mod encoder;

/// Tokio codec helper
#[cfg(feature = "tokio-udp")]
pub(crate) mod tokio;

use std::sync::Arc;

use futures::{Sink, Stream, StreamExt};
use log::{debug, trace};

use self::decoder::{DeFragmented, Deduplicated, FrameDecoded, Ordered};
use self::encoder::{Fragmented, FrameEncoded};
use crate::ack::SharedAck;
use crate::errors::CodecError;
use crate::packet::connected::{Frame, FrameBody, FrameSet, FramesMut};
use crate::utils::Logged;
use crate::{Message, RoleContext};

/// Codec config
#[derive(Clone, Copy, Debug)]
pub(crate) struct Config {
    /// Limit the max size of a parted frames set, 0 means no limit
    /// It will abort the split frame if the `parted_size` reaches limit.
    /// The maximum number of inflight parted frames is `max_parted_size` * `max_parted_count`nt
    pub(crate) max_parted_size: u32,
    /// Limit the max count of **all** parted frames sets from an address.
    /// It might cause client resending frames if the limit is reached.
    /// The maximum number of inflight parted frames is `max_parted_size` * `max_parted_count`nt
    pub(crate) max_parted_count: usize,
    /// Maximum ordered channel, the value should be less than 256
    pub(crate) max_channels: usize,
    // Limit the maximum deduplication gap for a connection, 0 means no limit.
    // Enable it to avoid D-DoS attack based on deduplication.
    pub(crate) max_dedup_gap: usize,
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
    fn decoded(
        self,
        config: Config,
        ack: SharedAck,
        role: RoleContext,
    ) -> impl Stream<Item = FrameBody>;
}

impl<F> Decoded for F
where
    F: Stream<Item = FrameSet<FramesMut>>,
{
    fn decoded(
        self,
        config: Config,
        ack: SharedAck,
        role: RoleContext,
    ) -> impl Stream<Item = FrameBody> {
        self.map(Ok)
            .deduplicated(config.max_dedup_gap, Arc::clone(&ack))
            .defragmented(config.max_parted_size, config.max_parted_count, ack)
            .ordered(config.max_channels)
            .frame_decoded()
            .logged_all(
                move |pack| {
                    trace!("[{role}] received packet: {:?}", pack);
                },
                move |err| {
                    debug!("[{role}] got codec error: {err} when decode packet");
                },
            )
    }
}

pub(crate) trait Encoded {
    fn frame_encoded(
        self,
        mtu: u16,
        config: Config,
    ) -> impl Sink<Message, Error = CodecError> + Sink<FrameBody, Error = CodecError>;
}

impl<F> Encoded for F
where
    F: Sink<Frame, Error = CodecError>,
{
    fn frame_encoded(
        self,
        mtu: u16,
        config: Config,
    ) -> impl Sink<Message, Error = CodecError> + Sink<FrameBody, Error = CodecError> {
        self.fragmented(mtu, config.max_channels).frame_encoded()
    }
}

/// Micro bench helper
#[cfg(feature = "micro-bench")]
pub mod micro_bench {
    use bytes::BytesMut;
    use rand::rngs::StdRng;
    use rand::seq::SliceRandom;
    use rand::{Rng, SeedableRng};

    use super::{Config, Decoded, FrameSet, FramesMut, Stream};
    use crate::ack::Acknowledgement;
    use crate::packet::connected::{Flags, Fragment, Frame, Ordered, Reliability};
    use crate::RoleContext;

    #[derive(Debug, Clone)]
    pub struct Options {
        pub frame_set_cnt: usize,
        pub frame_per_set: usize,
        pub duplicated_ratio: f32,
        pub unordered: bool,
        pub parted_size: usize,
        pub shuffle: bool,
        pub seed: u64,
        pub data: BytesMut,
    }

    impl Options {
        fn gen_inputs(&self) -> Vec<FrameSet<FramesMut>> {
            assert!(self.frame_per_set * self.frame_set_cnt % self.parted_size == 0);
            assert!(self.data.len() > self.parted_size);
            assert!(self.parted_size >= 1);
            let mut rng = StdRng::seed_from_u64(self.seed);
            let frames: FramesMut = std::iter::repeat(self.data.clone())
                .take(self.frame_per_set * self.frame_set_cnt)
                .enumerate()
                .map(|(idx, mut body)| {
                    let mut reliability = Reliability::Reliable;
                    let mut raw = 0;
                    let reliable_frame_index = Some(idx.into());
                    let mut fragment = None;
                    let mut ordered = None;
                    if self.parted_size > 1 {
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
                        reliability = Reliability::ReliableOrdered;
                        ordered = Some(Ordered {
                            frame_index: (idx / self.parted_size).into(),
                            channel: 0,
                        });
                    }
                    Frame {
                        flags: Flags::parse(((reliability as u8) << 5) | raw),
                        reliable_frame_index,
                        seq_frame_index: None,
                        ordered,
                        fragment,
                        body,
                    }
                })
                .flat_map(|frame| {
                    if self.duplicated_ratio > 0.
                        && rng.gen_ratio((self.duplicated_ratio * 100.0) as u32, 100)
                    {
                        return vec![frame.clone(), frame];
                    }
                    vec![frame]
                })
                .collect();
            let mut sets = frames
                .chunks(self.frame_per_set)
                .enumerate()
                .map(|(idx, chunk)| FrameSet {
                    seq_num: idx.into(),
                    set: chunk.to_vec(),
                })
                .collect::<Vec<_>>();
            if self.shuffle {
                sets.shuffle(&mut rng);
            }
            sets
        }

        pub fn input_data_cnt(&self) -> usize {
            self.frame_per_set * self.frame_set_cnt / self.parted_size
        }

        pub fn input_data_size(&self) -> usize {
            self.data.len() * self.input_data_cnt()
        }

        pub fn input_mtu(&self) -> usize {
            self.frame_per_set * self.data.len() / self.parted_size
        }
    }

    #[derive(Debug)]
    pub struct MicroBench {
        config: Config,
        #[cfg(test)]
        data: BytesMut,
        frame_sets: Vec<FrameSet<FramesMut>>,
    }

    impl MicroBench {
        pub fn new(option: Options) -> Self {
            Self {
                config: Config::default(),
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
            let ack = Acknowledgement::new_arc(RoleContext::Server);

            let stream = self.into_stream().decoded(config, ack, RoleContext::Server);
            #[futures_async_stream::for_await]
            for res in stream {
                let body = match res {
                    crate::packet::connected::FrameBody::User(body) => body,
                    _ => unreachable!("unexpected decoded result"),
                };
                assert_eq!(body.chunk(), data.chunk());
            }
        }

        #[allow(clippy::semicolon_if_nothing_returned)]
        pub async fn bench_decoded(self) {
            let config = self.config;
            let ack = Acknowledgement::new_arc(RoleContext::Server);

            let stream = self.into_stream().decoded(config, ack, RoleContext::Server);
            #[futures_async_stream::for_await]
            for _r in stream {}
        }

        fn into_stream(mut self) -> impl Stream<Item = FrameSet<FramesMut>> {
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
    async fn test_bench() {
        let opts = Options {
            frame_per_set: 8,
            frame_set_cnt: 100,
            duplicated_ratio: 0.1,
            unordered: true,
            parted_size: 4,
            shuffle: true,
            seed: 114514,
            data: BytesMut::from_iter(b"1145141919810"),
        };
        assert_eq!(opts.input_data_size(), 8 * 100 / 4 * "1145141919810".len());
        let bench = MicroBench::new(opts);
        bench.bench_decoded_checked().await;
    }
}
