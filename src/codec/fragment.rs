use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use futures::{Sink, Stream};
use lru::LruCache;
use pin_project_lite::pin_project;
use priority_queue::PriorityQueue;
use tracing::debug;

use crate::codec::PollPacket;
use crate::errors::CodecError;
use crate::packet::connected::{Fragment, Frame};
use crate::packet::{connected, PackId, Packet};

pin_project! {
    /// Defragment the frame set packet from stream (UdpFramed). Enable external consumption of
    /// continuous frame set packets.
    /// Notice that packets stream must pass this layer first, then go to the ack layer and timeout layer.
    /// Because this layer could abort the frames in frame set packet, and the ack layer will promise
    /// the client that we have received the frame set packet. Moreover, this layer needs to get the
    /// Disconnect packet sent by the timeout layer to clear the parted cache.
    pub(super) struct DeFragment<F> {
        #[pin]
        frame: F,
        // limit the max size of a parted frames set, 0 means no limit
        // it will abort the split frame if the parted_size reaches limit.
        limit_size: u32,
        // limit the max count of all parted frames sets from an address
        // it might cause client resending frames if the limit is reached.
        limit_parted: usize,
        parts: HashMap<SocketAddr, LruCache<u16, PriorityQueue<Frame, Reverse<u32>>>>,
        recv_buf: VecDeque<(Packet, SocketAddr)>,
    }
}

pub(super) trait DeFragmented: Sized {
    fn defragmented(self, limit_size: u32, limit_parted: usize) -> DeFragment<Self>;
}

impl<T> DeFragmented for T
where
    T: Stream<Item = Result<(Packet, SocketAddr), CodecError>>
        + Sink<(Packet, SocketAddr), Error = CodecError>,
{
    fn defragmented(self, limit_size: u32, limit_parted: usize) -> DeFragment<Self> {
        const DEFAULT_RECV_BUF_CAP: usize = 256;

        DeFragment {
            frame: self,
            limit_size,
            limit_parted,
            parts: HashMap::new(),
            recv_buf: VecDeque::with_capacity(DEFAULT_RECV_BUF_CAP),
        }
    }
}

impl<F> Stream for DeFragment<F>
where
    F: Stream<Item = Result<(Packet, SocketAddr), CodecError>>,
{
    type Item = Result<(Packet, SocketAddr), CodecError>;

    // TODO
    // Usually, there are a multiple frames frame set packet, and all frames are partitioned or not
    // partitioned. These two kinds of frames are usually not mixed in the same frame
    // set. Verify this hypothesis and optimize the code below. Maybe we could remove recv_buf
    // here.
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        loop {
            // try empty buffer
            if let Some(buf_res) = this.recv_buf.pop_front() {
                return Poll::Ready(Some(Ok(buf_res)));
            }

            let (packet, addr) = match this.frame.as_mut().poll_packet(cx) {
                Ok(v) => v,
                Err(poll) => return poll,
            };

            let Packet::Connected(connected::Packet::FrameSet(frame_set)) = packet else {
                return Poll::Ready(Some(Ok((packet, addr))));
            };

            let mut single_frames = Vec::with_capacity(frame_set.frames.len());
            for frame in frame_set.frames {
                if let Some(Fragment {
                    parted_size,
                    parted_id,
                    parted_index,
                }) = frame.fragment.clone()
                {
                    // promise that parted_index is always less than parted_size
                    if parted_index >= parted_size {
                        return Poll::Ready(Some(Err(CodecError::PartedFrame(format!(
                            "parted_index {} >= parted_size {}",
                            parted_index, parted_size
                        )))));
                    }
                    if *this.limit_size != 0 && parted_size > *this.limit_size {
                        return Poll::Ready(Some(Err(CodecError::PartedFrame(format!(
                            "parted_size {} exceed limit_size {}",
                            parted_size, *this.limit_size
                        )))));
                    }
                    let parts = this.parts.entry(addr).or_insert_with(|| {
                        LruCache::new(
                            NonZeroUsize::new(*this.limit_parted).expect("limit_parted > 0"),
                        )
                    });
                    let frames_queue = parts.get_or_insert_mut(parted_id, || {
                        debug!("new parted_id {parted_id} from {addr}",);
                        PriorityQueue::with_capacity(parted_size as usize)
                    });
                    frames_queue.push(frame, Reverse(parted_index));
                    if frames_queue.len() < parted_size as usize {
                        continue;
                    }
                    // parted_index is always less than parted_size, frames_queue length
                    // reaches parted_size and frame is hashed by parted_index, so here we
                    // get the complete frames vector
                    let frames: Vec<Frame> = parts
                        .pop(&parted_id)
                        .unwrap_or_else(|| {
                            unreachable!("parted_id {parted_id} should be set before")
                        })
                        .into_sorted_vec();
                    this.recv_buf.push_back((
                        Packet::Connected(connected::Packet::FrameSet(connected::FrameSet {
                            frames,
                            ..frame_set
                        })),
                        addr,
                    ));
                    continue;
                }
                single_frames.push(frame);
            }
            if !single_frames.is_empty() {
                this.recv_buf.push_back((
                    Packet::Connected(connected::Packet::FrameSet(connected::FrameSet {
                        frames: single_frames,
                        ..frame_set
                    })),
                    addr,
                ));
            }
        }
    }
}

impl<F> Sink<(Packet, SocketAddr)> for DeFragment<F>
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
        (packet, addr): (Packet, SocketAddr),
    ) -> Result<(), Self::Error> {
        let this = self.project();
        if let Packet::Connected(connected::Packet::FrameSet(frame_set)) = &packet {
            if matches!(frame_set.inner_pack_id()?, PackId::DisconnectNotification) {
                debug!("disconnect from {}, clean it's frame parts buffer", addr);
                this.parts.remove(&addr);
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
