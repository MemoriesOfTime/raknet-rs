mod dedup;
mod fragment;
mod ordered;

use std::borrow::Borrow;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::{Buf, Bytes, BytesMut};
use derive_builder::Builder;
use futures::{Sink, Stream};
use pin_project_lite::pin_project;
use tokio::net::UdpSocket;
use tokio_util::codec::{Decoder, Encoder};
use tokio_util::udp::UdpFramed;
use tracing::{debug, trace};

use self::ordered::Ordered;
use crate::codec::dedup::Deduplicated;
use crate::codec::fragment::DeFragmented;
use crate::errors::CodecError;
use crate::packet::Packet;

/// Codec config
#[derive(Clone, Copy, Debug, Builder)]
pub struct CodecConfig {
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

impl Default for CodecConfig {
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

pub(crate) trait Framed: Sized {
    fn framed(
        self,
        config: CodecConfig,
    ) -> impl Stream<Item = (Packet<Bytes>, SocketAddr)>
           + Sink<(Packet<Bytes>, SocketAddr), Error = CodecError>;
}

impl<T> Framed for T
where
    T: Borrow<UdpSocket> + Sized,
{
    fn framed(
        self,
        config: CodecConfig,
    ) -> impl Stream<Item = (Packet<Bytes>, SocketAddr)>
           + Sink<(Packet<Bytes>, SocketAddr), Error = CodecError> {
        let frame = UdpFramed::new(self, Codec)
            .deduplicated(config.max_dedup_gap) // BytesMut -> BytesMut
            .defragmented(config.max_parted_size, config.max_parted_count) // BytesMut -> Bytes
            .ordered(config.max_channels); // Bytes -> Bytes
        LoggedCodec { frame }
    }
}

/// The raknet codec
pub(crate) struct Codec;

impl<B: Buf> Encoder<Packet<B>> for Codec {
    type Error = CodecError;

    fn encode(&mut self, item: Packet<B>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        item.write(dst);
        Ok(())
    }
}

impl Decoder for Codec {
    type Error = CodecError;
    // we might want to update the package during codec
    type Item = Packet<BytesMut>;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        Packet::read(src)
    }
}

pin_project! {
    /// Log the error of the packet codec while reading.
    /// We probably don't care about the codec error while decoding request packets.
    struct LoggedCodec<F> {
        #[pin]
        frame: F,
    }
}

impl<F, B: Buf> Stream for LoggedCodec<F>
where
    F: Stream<Item = Result<(Packet<B>, SocketAddr), CodecError>>,
{
    type Item = (Packet<B>, SocketAddr);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            let Some(res) = ready!(this.frame.as_mut().poll_next(cx)) else {
                return Poll::Ready(None);
            };
            let (packet, addr) = match res {
                Ok((packet, addr)) => (packet, addr),
                Err(err) => {
                    debug!("raknet codec error: {err}, ignore this packet");
                    continue;
                }
            };
            trace!("received packet: {:?}, from: {addr}", packet.pack_id());
            return Poll::Ready(Some((packet, addr)));
        }
    }
}

/// Propagate sink for `LoggedCodec`
impl<F, B: Buf> Sink<(Packet<B>, SocketAddr)> for LoggedCodec<F>
where
    F: Sink<(Packet<B>, SocketAddr)>,
{
    type Error = F::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: (Packet<B>, SocketAddr)) -> Result<(), Self::Error> {
        self.project().frame.start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_close(cx)
    }
}

trait PollPacket<B: Buf> {
    #[allow(clippy::type_complexity)] // not too bad
    fn poll_packet<T>(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Result<(Packet<B>, SocketAddr), Poll<Option<Result<T, CodecError>>>>;
}

impl<F, B: Buf> PollPacket<B> for F
where
    F: Stream<Item = Result<(Packet<B>, SocketAddr), CodecError>>,
{
    fn poll_packet<T>(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Result<(Packet<B>, SocketAddr), Poll<Option<Result<T, CodecError>>>> {
        let res = match self.poll_next(cx) {
            Poll::Ready(res) => res,
            Poll::Pending => return Err(Poll::Pending),
        };
        let Some(res) = res else {
            return Err(Poll::Ready(None));
        };
        res.map_err(|err| Poll::Ready(Some(Err(err))))
    }
}

#[cfg(test)]
mod test {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use bytes::Bytes;
    use futures::{SinkExt, StreamExt};
    use tokio::net::UdpSocket;
    use tracing_test::traced_test;

    use crate::codec::{CodecConfig, Framed};
    use crate::packet::{unconnected, Packet};

    fn unconnected_ping() -> Packet<Bytes> {
        Packet::Unconnected(unconnected::Packet::UnconnectedPing {
            send_timestamp: 0,
            magic: (),
            client_guid: 114514,
        })
    }

    #[tokio::test]
    #[traced_test]
    async fn test_truncated_will_not_panic() {
        let socket = Arc::new(
            UdpSocket::bind("0.0.0.0:0".parse::<SocketAddr>().unwrap())
                .await
                .unwrap(),
        );
        let listen_addr = socket.local_addr().unwrap();
        let mut framed = socket.framed(CodecConfig::default()).buffer(10);
        let send_socket = UdpSocket::bind("0.0.0.0:0".parse::<SocketAddr>().unwrap())
            .await
            .unwrap();
        send_socket.send_to(&[1], listen_addr).await.unwrap();
        send_socket
            .framed(CodecConfig::default())
            .send((unconnected_ping(), listen_addr))
            .await
            .unwrap();
        let (packet, _) = framed.next().await.unwrap();
        assert_eq!(packet, unconnected_ping());
    }
}

#[cfg(feature = "micro-bench")]
pub mod micro_bench {
    use rand::seq::SliceRandom;
    use rand::Rng;

    use super::{
        BytesMut, Codec, CodecConfig, CodecError, Context, DeFragmented, Decoder, Deduplicated,
        Encoder, LoggedCodec, Packet, Pin, Poll, SocketAddr, Stream,
    };
    use crate::packet::connected::{self, Flags, Fragment, Frame, FrameSet, Ordered, Uint24le};

    fn frame_set<'a, T: AsRef<str> + 'a>(
        idx: impl IntoIterator<Item = &'a (((u32, u16, u32, T), u32), u32)>,
    ) -> Packet<BytesMut> {
        Packet::Connected(connected::Packet::FrameSet(FrameSet {
            seq_num: Uint24le(0),
            frames: idx
                .into_iter()
                .map(
                    |(
                        ((parted_size, parted_id, parted_index, body), frame_index),
                        reliable_index,
                    )| Frame {
                        flags: Flags::parse(0b011_11100),
                        reliable_frame_index: Some(Uint24le(*reliable_index)),
                        seq_frame_index: None,
                        ordered: Some(Ordered {
                            frame_index: Uint24le(*frame_index),
                            channel: 0,
                        }),
                        fragment: Some(Fragment {
                            parted_size: *parted_size,
                            parted_id: *parted_id,
                            parted_index: *parted_index,
                        }),
                        body: BytesMut::from(body.as_ref()),
                    },
                )
                .collect(),
        }))
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::missing_panics_doc)]
    pub fn micro_bench_codec_gen_data<R: Rng>(
        pack_size: usize,
        chunk_size: usize,
        parted_shuffle: bool,
        reliable_shuffle: bool,
        packets_shuffle: bool,
        random_frame_dup_threshold: f32,
        random_chunk_dup_threshold: f32,
        rng: &mut R,
    ) -> Vec<BytesMut> {
        assert_eq!(pack_size % 5, 0);
        assert!(chunk_size <= (pack_size / 5));

        const BODY_0: &str = "A";
        const BODY_1: &str = include_str!("../../benches/data/body-short.txt");
        const BODY_2: &str = include_str!("../../benches/data/body-medium-short.txt");
        const BODY_3: &str = include_str!("../../benches/data/body-medium.txt");
        const BODY_4: &str = include_str!("../../benches/data/body-large.txt");

        let parted_size = (pack_size / 5) as u32;

        let mut packets = [
            (0, BODY_0),
            (1, BODY_1),
            (2, BODY_2),
            (3, BODY_3),
            (4, BODY_4),
        ]
        .into_iter()
        .flat_map(|(parted_id, body)| {
            let mut parted_slice = (0..parted_size).collect::<Vec<_>>();
            if parted_shuffle {
                parted_slice.shuffle(rng);
            }
            let from = u32::from(parted_id) * parted_size;
            let to = from + parted_size;
            let mut reliable_index = (from..to).collect::<Vec<_>>();
            if reliable_shuffle {
                reliable_index.shuffle(rng);
            }

            std::iter::repeat(parted_size)
                .zip(parted_slice)
                .map(|(l, r)| (l, parted_id, r, body))
                .zip(std::iter::repeat(parted_id as u32))
                .zip(reliable_index)
                .flat_map(|item| {
                    if rng.gen::<f32>() < random_frame_dup_threshold {
                        vec![item, item]
                    } else {
                        vec![item]
                    }
                })
                .collect::<Vec<_>>()
                .chunks(chunk_size)
                .map(frame_set)
                .flat_map(|pack| {
                    if rng.gen::<f32>() < random_chunk_dup_threshold {
                        vec![pack.clone(), pack]
                    } else {
                        vec![pack]
                    }
                })
                .collect::<Vec<_>>()
        })
        .map(|pack| {
            let mut bytes = BytesMut::new();
            Codec
                .encode(pack, &mut bytes)
                .expect("encoder should return error");
            bytes
        })
        .collect::<Vec<_>>();

        if packets_shuffle {
            packets.shuffle(rng);
        }

        assert!(packets.len() >= pack_size / chunk_size);

        packets
    }

    #[allow(clippy::semicolon_if_nothing_returned)] // false positive
    #[allow(clippy::too_many_arguments)] // bench only
    pub async fn micro_bench_codec_decode(data: Vec<BytesMut>, config: CodecConfig) {
        use crate::codec::ordered::Ordered;

        struct BenchFrame(Vec<BytesMut>);

        impl Stream for BenchFrame {
            type Item = Result<(Packet<BytesMut>, SocketAddr), CodecError>;

            fn poll_next(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Self::Item>> {
                while let Some(mut pack) = self.0.pop() {
                    let res = Codec.decode(&mut pack);
                    match res {
                        Ok(Some(packet)) => {
                            return Poll::Ready(Some(Ok((
                                packet,
                                SocketAddr::new(
                                    std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
                                    8080,
                                ),
                            ))))
                        }
                        Ok(None) => continue,
                        Err(err) => return Poll::Ready(Some(Err(err))),
                    }
                }
                Poll::Ready(None)
            }
        }

        let codec = LoggedCodec {
            frame: BenchFrame(data)
                .deduplicated(config.max_dedup_gap)
                .defragmented(config.max_parted_size, config.max_parted_count)
                .ordered(config.max_channels),
        };

        #[futures_async_stream::for_await]
        for (pack, _) in codec {
            tracing::debug!("\n\n{pack:?}\n\n");
        }
    }

    #[cfg(test)]
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn test_micro_bench_works() {
        {
            let data = micro_bench_codec_gen_data(
                100,
                20,
                true,
                true,
                true,
                0.5,
                0.5,
                &mut rand::thread_rng(),
            );
            micro_bench_codec_decode(data, CodecConfig::default()).await;
        }

        {
            let data = micro_bench_codec_gen_data(
                1000,
                20,
                true,
                true,
                true,
                0.5,
                0.5,
                &mut rand::thread_rng(),
            );
            micro_bench_codec_decode(data, CodecConfig::default()).await;
        }
    }
}
