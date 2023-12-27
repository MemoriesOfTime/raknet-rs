use std::collections::HashMap;
use std::net::SocketAddr;
use std::ops::AddAssign;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Buf;
use futures::{Sink, Stream};
use pin_project_lite::pin_project;
use tracing::debug;

use super::PollPacket;
use crate::errors::CodecError;
use crate::packet::connected::Frame;
use crate::packet::{connected, PackId, Packet};

const INITIAL_ORDERING_MAP_CAP: usize = 64;

struct Ordering<B: Buf> {
    map: HashMap<u32, Frame<B>>,
    read: u32,
}

impl<B: Buf> Default for Ordering<B> {
    fn default() -> Self {
        Self {
            map: HashMap::with_capacity(INITIAL_ORDERING_MAP_CAP),
            read: 0,
        }
    }
}

pin_project! {
    // Ordering layer, ordered the packets based on ordering_frame_index.
    // This layer should be placed behind the ack layer because it may
    // modify the packet sent from the top layer. The ack layer have to record
    // all packets and resend them if need.
    pub(super) struct Order<F, B: Buf> {
        #[pin]
        frame: F,
        // Max ordered channel that will be used in detailed protocol
        max_channels: usize,
        ordering: HashMap<SocketAddr, Vec<Ordering<B>>>,
    }
}

pub(super) trait Ordered: Sized {
    fn ordered<B: Buf>(self, max_channels: usize) -> Order<Self, B>;
}

impl<T> Ordered for T {
    fn ordered<B: Buf>(self, max_channels: usize) -> Order<Self, B> {
        assert!(
            max_channels < usize::from(u8::MAX),
            "max channels should not be larger than u8::MAX"
        );
        assert!(max_channels > 0, "max_channels > 0");

        Order {
            frame: self,
            max_channels,
            ordering: HashMap::new(),
        }
    }
}

impl<F, B: Buf> Stream for Order<F, B>
where
    F: Stream<Item = Result<(Packet<B>, SocketAddr), CodecError>>,
{
    type Item = Result<(Packet<B>, SocketAddr), CodecError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        loop {
            let (packet, addr) = match this.frame.as_mut().poll_packet(cx) {
                Ok(v) => v,
                Err(poll) => return poll,
            };

            let Packet::Connected(connected::Packet::FrameSet(frame_set)) = packet else {
                return Poll::Ready(Some(Ok((packet, addr))));
            };

            let mut frames = None;
            let frames_len = frame_set.frames.len();
            for frame in frame_set.frames {
                if let Some(connected::Ordered {
                    frame_index,
                    channel,
                }) = frame.ordered.clone()
                {
                    let channel = usize::from(channel);
                    if channel >= *this.max_channels {
                        return Poll::Ready(Some(Err(CodecError::OrderedFrame(format!(
                            "channel {} >= max_channels {}",
                            channel, *this.max_channels
                        )))));
                    }
                    let ordering = this
                        .ordering
                        .entry(addr)
                        .or_insert_with(|| {
                            std::iter::repeat_with(Ordering::default)
                                .take(*this.max_channels)
                                .collect()
                        })
                        .get_mut(channel)
                        .expect("channel < max_channels");

                    match frame_index.0.cmp(&ordering.read) {
                        std::cmp::Ordering::Less => {
                            debug!("ignore old ordered frame index {frame_index}");
                            continue;
                        }
                        std::cmp::Ordering::Greater => {
                            ordering.map.insert(frame_index.0, frame);
                            continue;
                        }
                        std::cmp::Ordering::Equal => {
                            ordering.read.add_assign(1);
                        }
                    }

                    // then we got a frame index equal to read index, we could read it
                    frames
                        .get_or_insert_with(|| Vec::with_capacity(frames_len))
                        .push(frame);

                    // check if we could read more
                    while let Some(next) = ordering.map.remove(&ordering.read) {
                        ordering.read.add_assign(1);
                        frames
                            .get_or_insert_with(|| Vec::with_capacity(frames_len))
                            .push(next);
                    }

                    // we cannot read anymore
                    continue;
                }
                // the frameset which does not require ordered
                frames
                    .get_or_insert_with(|| Vec::with_capacity(frames_len))
                    .push(frame);
            }
            if let Some(frames) = frames {
                return Poll::Ready(Some(Ok((
                    Packet::Connected(connected::Packet::FrameSet(connected::FrameSet {
                        frames,
                        ..frame_set
                    })),
                    addr,
                ))));
            }
        }
    }
}

impl<F, B: Buf> Sink<(Packet<B>, SocketAddr)> for Order<F, B>
where
    F: Sink<(Packet<B>, SocketAddr), Error = CodecError>,
{
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_ready(cx)
    }

    fn start_send(
        self: Pin<&mut Self>,
        (mut packet, addr): (Packet<B>, SocketAddr),
    ) -> Result<(), Self::Error> {
        let this = self.project();

        if let Packet::Connected(connected::Packet::FrameSet(frame_set)) = &mut packet {
            if matches!(frame_set.inner_pack_id()?, PackId::DisconnectNotification) {
                debug!("disconnect from {}, clean it's ordering buffer", addr);
                this.ordering.remove(&addr);
            }
        };
        this.frame.start_send((packet, addr))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_close(cx)
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use bytes::Bytes;
    use futures::StreamExt;
    use futures_async_stream::stream;

    use super::Order;
    use crate::errors::CodecError;
    use crate::packet::connected::{self, Flags, Frame, FrameSet, Ordered, Uint24le};
    use crate::packet::Packet;

    fn frame_set(idx: impl IntoIterator<Item = (u8, u32)>) -> Packet<Bytes> {
        Packet::Connected(connected::Packet::FrameSet(FrameSet {
            seq_num: Uint24le(0),
            frames: idx
                .into_iter()
                .map(|(channel, frame_index)| Frame {
                    flags: Flags::parse(0b011_11100),
                    reliable_frame_index: None,
                    seq_frame_index: None,
                    ordered: Some(Ordered {
                        frame_index: Uint24le(frame_index),
                        channel,
                    }),
                    fragment: None,
                    body: Bytes::new(),
                })
                .collect(),
        }))
    }

    #[tokio::test]
    async fn test_ordered_works() {
        let addr1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let addr2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8081);
        let frame = {
            #[stream]
            async {
                yield (frame_set([(0, 1), (0, 0), (0, 2), (0, 4), (0, 3)]), addr1);
                yield (frame_set([(1, 1)]), addr1);

                yield (frame_set([(3, 1), (0, 0), (0, 2), (0, 1), (0, 3)]), addr2);
                yield (frame_set([(3, 0)]), addr2);
            }
        };
        tokio::pin!(frame);

        let mut ordered = Order {
            frame: frame.map(Ok),
            max_channels: 10,
            ordering: HashMap::new(),
        };

        assert_eq!(
            ordered.next().await.unwrap().unwrap(),
            (frame_set([(0, 0), (0, 1), (0, 2), (0, 3), (0, 4)]), addr1)
        );
        assert_eq!(
            ordered.next().await.unwrap().unwrap(),
            (frame_set([(0, 0), (0, 1), (0, 2), (0, 3)]), addr2)
        );
        assert_eq!(
            ordered.next().await.unwrap().unwrap(),
            (frame_set([(3, 0), (3, 1)]), addr2)
        );
        assert!(ordered.next().await.is_none());
    }

    #[tokio::test]
    async fn test_ordered_channel_exceed() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let frame = {
            #[stream]
            async {
                yield (frame_set([(10, 1)]), addr);
            }
        };
        tokio::pin!(frame);

        let mut ordered = Order {
            frame: frame.map(Ok),
            max_channels: 10,
            ordering: HashMap::new(),
        };

        assert!(matches!(
            ordered.next().await.unwrap().unwrap_err(),
            CodecError::OrderedFrame(_)
        ));
    }
}
