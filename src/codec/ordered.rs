use std::collections::HashMap;
use std::net::SocketAddr;
use std::ops::AddAssign;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use futures::{Sink, Stream};
use pin_project_lite::pin_project;
use tracing::debug;

use super::PollPacket;
use crate::errors::CodecError;
use crate::packet::connected::{Frame, Uint24le};
use crate::packet::{connected, PackId, Packet};

const INITIAL_ORDERING_MAP_CAP: usize = 64;
const DEFAULT_SEND_CHANNEL: u8 = 0;

pin_project! {
    pub(super) struct Order<F> {
        #[pin]
        frame: F,
        // Max ordered channel that will be used in detailed protocol
        max_channels: usize,
        // Whether to enable ordered send when there is no order option in frame before
        default_ordered_send: bool,
        ordering: HashMap<SocketAddr, Vec<HashMap<u32, Frame>>>,
        read: HashMap<SocketAddr, Vec<u32>>,
        sent: HashMap<SocketAddr, Vec<u32>>,
    }
}

pub(super) trait Ordered: Sized {
    fn ordered(self, max_channels: usize, default_ordered_send: bool) -> Order<Self>;
}

impl<T> Ordered for T
where
    T: Stream<Item = Result<(Packet, SocketAddr), CodecError>>
        + Sink<(Packet, SocketAddr), Error = CodecError>,
{
    fn ordered(self, max_channels: usize, default_ordered_send: bool) -> Order<Self> {
        assert!(
            max_channels < usize::from(u8::MAX),
            "max channels should not be larger than u8::MAX"
        );
        assert!(max_channels > 0, "max_channels > 0");

        Order {
            frame: self,
            max_channels,
            ordering: HashMap::new(),
            default_ordered_send,
            read: HashMap::new(),
            sent: HashMap::new(),
        }
    }
}

impl<F> Stream for Order<F>
where
    F: Stream<Item = Result<(Packet, SocketAddr), CodecError>>,
{
    type Item = Result<(Packet, SocketAddr), CodecError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

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
                let ordering_map = this
                    .ordering
                    .entry(addr)
                    .or_insert_with(|| {
                        std::iter::repeat_with(|| HashMap::with_capacity(INITIAL_ORDERING_MAP_CAP))
                            .take(*this.max_channels)
                            .collect()
                    })
                    .get_mut(channel)
                    .expect("channel < max_channels");
                let read_index = this
                    .read
                    .entry(addr)
                    .or_insert_with(|| std::iter::repeat(0).take(*this.max_channels).collect())
                    .get_mut(channel)
                    .expect("channel < max_channels");

                match frame_index.0.cmp(read_index) {
                    std::cmp::Ordering::Less => {
                        debug!("ignore old frame index {frame_index}");
                        continue;
                    }
                    std::cmp::Ordering::Greater => {
                        ordering_map.insert(frame_index.0, frame);
                        continue;
                    }
                    std::cmp::Ordering::Equal => {
                        read_index.add_assign(1);
                    }
                }

                // then we got a frame index equal to read index, we could read it
                frames
                    .get_or_insert_with(|| Vec::with_capacity(frames_len))
                    .push(frame);

                // check if we could read more
                while let Some(next) = ordering_map.remove(read_index) {
                    read_index.add_assign(1);
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
        // notify the ack layer to acknowledge this frameset but with empty frames
        Poll::Ready(Some(Ok((
            Packet::Connected(connected::Packet::FrameSet(connected::FrameSet {
                frames: vec![],
                ..frame_set
            })),
            addr,
        ))))
    }
}

impl<F> Sink<(Packet, SocketAddr)> for Order<F>
where
    F: Sink<(Packet, SocketAddr), Error = CodecError>,
{
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        this.frame.poll_ready(cx)
    }

    fn start_send(
        self: Pin<&mut Self>,
        (mut packet, addr): (Packet, SocketAddr),
    ) -> Result<(), Self::Error> {
        let this = self.project();

        if let Packet::Connected(connected::Packet::FrameSet(frame_set)) = &mut packet {
            for frame in &mut frame_set.frames {
                let Some(channel) = frame.ordered.as_ref().map_or(
                    this.default_ordered_send.then_some(DEFAULT_SEND_CHANNEL),
                    |ordered| Some(ordered.channel),
                ) else {
                    continue;
                };
                let sent = this
                    .sent
                    .entry(addr)
                    .or_insert_with(|| std::iter::repeat(0).take(*this.max_channels).collect())
                    .get_mut(usize::from(channel))
                    .expect("channel < max_channels");
                frame.ordered = Some(connected::Ordered {
                    frame_index: Uint24le(*sent),
                    channel,
                });
                sent.add_assign(1);
            }
            if matches!(frame_set.inner_pack_id()?, PackId::DisconnectNotification) {
                debug!("disconnect from {}, clean it's frame parts buffer", addr);
                this.ordering.remove(&addr);
                this.read.remove(&addr);
                this.sent.remove(&addr);
            }
        };
        this.frame.start_send((packet, addr))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        this.frame.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.poll_flush(cx))?;
        Poll::Ready(Ok(()))
    }
}
