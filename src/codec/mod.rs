/// Raw frames codec
pub(crate) mod frame;

/// Frames pipeline decoder
mod decoder;

/// Frames pipeline encoder
mod encoder;

/// Tokio codec helper
#[cfg(feature = "tokio-rt")]
pub(crate) mod tokio;

use std::io;
use std::net::SocketAddr;
use std::task::{Context, Poll};

use bytes::BytesMut;
use futures::{Sink, Stream, StreamExt};

use self::decoder::{BodyDecoded, DeFragmented, Deduplicated, Ordered, TracePending};
use self::encoder::{BodyEncoded, Fragmented};
use crate::errors::CodecError;
use crate::link::SharedLink;
use crate::packet::connected::{Frame, FrameBody, FrameSet, FramesMut};
use crate::{Message, Priority};

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
}

impl Default for Config {
    fn default() -> Self {
        // recommend configuration
        Self {
            max_parted_size: 256,
            max_parted_count: 256,
            max_channels: 1,
        }
    }
}

/// Abstract async socket
/// It's used to decode/encode raw frames from/to the socket.
pub(crate) trait AsyncSocket: Send + Sync + Unpin + Clone + 'static {
    fn poll_recv_from(
        &self,
        cx: &mut Context<'_>,
        buf: &mut BytesMut,
    ) -> Poll<io::Result<SocketAddr>>;

    fn poll_send_to(
        &self,
        cx: &mut Context<'_>,
        buf: &[u8],
        target: SocketAddr,
    ) -> Poll<io::Result<usize>>;
}

/// Frames pipeline decoder
/// It will convert the stream of raw frames into defragmented, deduplicated and ordered frames.
pub(crate) trait Decoded {
    fn frame_decoded(self, config: Config) -> impl Stream<Item = Result<FrameBody, CodecError>>;
}

impl<F> Decoded for F
where
    F: Stream<Item = FrameSet<FramesMut>>,
{
    fn frame_decoded(self, config: Config) -> impl Stream<Item = Result<FrameBody, CodecError>> {
        self.map(Ok)
            .trace_pending()
            .deduplicated()
            .defragmented(config.max_parted_size, config.max_parted_count)
            .ordered(config.max_channels)
            .body_decoded()
    }
}

/// Frames pipeline encoder
/// It will sink the messages/frame bodies into fragmented frames.
pub(crate) trait Encoded {
    fn frame_encoded(
        self,
        mtu: u16,
        config: Config,
        link: SharedLink,
    ) -> impl Sink<Message, Error = io::Error> + Sink<FrameBody, Error = io::Error>;
}

impl<F> Encoded for F
where
    F: Sink<(Priority, Frame), Error = io::Error>,
{
    fn frame_encoded(
        self,
        mtu: u16,
        config: Config,
        link: SharedLink,
    ) -> impl Sink<Message, Error = io::Error> + Sink<FrameBody, Error = io::Error> {
        self.fragmented(mtu as usize, config.max_channels)
            .body_encoded(link)
    }
}

/// Micro bench helper
#[cfg(feature = "micro-bench")]
pub mod micro_bench {
    use std::collections::VecDeque;
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use bytes::{Bytes, BytesMut};
    use futures::{Sink, StreamExt};
    use rand::rngs::StdRng;
    use rand::seq::SliceRandom;
    use rand::{Rng, SeedableRng};

    use super::{Config, Decoded, Fragmented, FrameSet, FramesMut, Stream};
    use crate::packet::connected::Frame;
    use crate::packet::FRAME_SET_HEADER_SIZE;
    use crate::{Message, Priority};

    #[derive(Debug, Clone)]
    pub struct BenchOpts {
        pub datagrams: Vec<Bytes>,
        pub seed: u64,
        pub dup_ratio: f32,
        pub shuffle_ratio: f32,
        pub mtu: usize,
    }

    #[derive(Debug, Default)]
    struct SinkDst {
        buf: VecDeque<Frame<BytesMut>>,
    }

    impl Sink<(Priority, Frame)> for &mut SinkDst {
        type Error = io::Error;

        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(
            self: Pin<&mut Self>,
            (_, frame): (Priority, Frame),
        ) -> Result<(), Self::Error> {
            self.get_mut().buf.push_back(Frame {
                body: BytesMut::from(frame.body),
                ..frame
            });
            Ok(())
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    #[allow(missing_debug_implementations)]
    pub struct Inputs {
        stream: Pin<Box<dyn Stream<Item = FrameSet<FramesMut>>>>,
    }

    /// Run/Test codec benchmarks
    #[allow(clippy::missing_panics_doc)]
    pub async fn run_bench(inputs: Inputs) {
        let mut decoding = inputs.stream.frame_decoded(Config::default());
        while let Some(r) = decoding.next().await {
            assert!(r.is_ok());
        }
    }

    impl BenchOpts {
        #[allow(clippy::missing_panics_doc)]
        pub fn gen_inputs(&self) -> Inputs {
            let mut frames = SinkDst::default();
            let mut rng = StdRng::seed_from_u64(self.seed);
            tokio::pin! {
                let fragmented = (&mut frames).fragmented(self.mtu, 1);
            }
            for datagram in self.datagrams.clone() {
                fragmented
                    .as_mut()
                    .start_send(Message::new(datagram)) // reliable ordered by default
                    .unwrap();
            }
            let mut sets = Vec::new();
            let mut remain = self.mtu - FRAME_SET_HEADER_SIZE;
            let mut set: Option<FramesMut> = None;
            while let Some(frame) = frames.buf.front() {
                if remain >= frame.size() {
                    remain -= frame.size();
                    set.get_or_insert_default()
                        .push(frames.buf.pop_front().unwrap());
                    continue;
                }
                remain = self.mtu - FRAME_SET_HEADER_SIZE;

                if self.dup_ratio > 0. && rng.gen_ratio((self.dup_ratio * 100.0) as u32, 100) {
                    sets.push(FrameSet {
                        seq_num: sets.len().into(),
                        set: set.clone().take().unwrap(),
                    });
                }
                sets.push(FrameSet {
                    seq_num: sets.len().into(),
                    set: set.take().unwrap(),
                });
            }
            if let Some(set) = set {
                sets.push(FrameSet {
                    seq_num: sets.len().into(),
                    set,
                });
            }

            let len = sets.len();
            if self.shuffle_ratio > 0. {
                sets.partial_shuffle(&mut rng, (len as f32 * self.shuffle_ratio) as usize);
            }
            let stream = {
                #[futures_async_stream::stream]
                async move {
                    let mut sets = VecDeque::from(sets);
                    while let Some(frame_set) = sets.pop_front() {
                        yield frame_set;
                    }
                }
            };
            Inputs {
                stream: Box::pin(stream),
            }
        }

        pub fn bytes(&self) -> u64 {
            self.datagrams.iter().map(|b| b.len() as u64).sum()
        }

        pub fn elements(&self) -> u64 {
            self.datagrams.len() as u64
        }

        #[cfg(test)]
        async fn test_bench(self, inputs: Inputs) {
            let mut len = self.datagrams.len();
            let mut datagrams = VecDeque::from(self.datagrams);
            let mut decoding = inputs.stream.frame_decoded(Config::default());
            while let Some(r) = decoding.next().await {
                assert!(r.is_ok());
                len -= 1;
                let body = match r.unwrap() {
                    crate::packet::connected::FrameBody::User(body) => body,
                    _ => unreachable!("unexpected decoded result"),
                };
                log::debug!("decoded: {:?}", body);
                assert_eq!(body, datagrams.pop_front().unwrap());
            }
            assert_eq!(len, 0);
        }
    }

    #[cfg(test)]
    #[tokio::test]
    async fn test_bench() {
        use crate::utils::tests::test_trace_log_setup;

        let _guard = test_trace_log_setup();
        let opts = BenchOpts {
            datagrams: vec![
                Bytes::from_static(b"hello"),
                Bytes::from_static(b"world"),
                Bytes::from_static(b"!"),
            ],
            seed: 114514,
            dup_ratio: 0.6,
            shuffle_ratio: 0.6,
            mtu: 30,
        };
        assert_eq!(opts.bytes(), 11);
        assert_eq!(opts.elements(), 3);
        let inputs = opts.gen_inputs();
        opts.test_bench(inputs).await;
    }
}
