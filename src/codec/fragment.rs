use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use futures::{Sink, Stream};
use pin_project_lite::pin_project;
use priority_queue::PriorityQueue;
use tracing::debug;

use crate::errors::CodecError;
use crate::packet::connected::{Fragment, Frame};
use crate::packet::{connected, PackId, Packet};

pin_project! {
    /// Fragment/Defragment the frame set packet from sink/stream (UdpFramed). Enable external production
    /// and consumption of continuous frame set packets.
    pub(super) struct FragmentFrame<F> {
        #[pin]
        frame: F,
        // limit the max size of a parted frames set, 0 means no limit
        // it will cause client resending frames if the limit is reached.
        limit_size: u32,
        // limit the max count of all parted frames sets from an address, 0 means no limit.
        // it will cause client resending frames if the limit is reached.
        limit_parted: usize,
        parts: HashMap<SocketAddr, HashMap<u16, PriorityQueue<Frame, Reverse<u32>>>>,  // TODO apply LRU to delete old addr
        recv_buf: VecDeque<(Packet, SocketAddr)>,
    }
}

pub(super) trait Fragmented: Sized {
    fn fragmented(self, limit_size: u32, limit_parted: usize) -> FragmentFrame<Self>;
}

impl<T> Fragmented for T
where
    T: Stream<Item = Result<(Packet, SocketAddr), CodecError>>
        + Sink<(Packet, SocketAddr), Error = CodecError>,
{
    fn fragmented(self, limit_size: u32, limit_parted: usize) -> FragmentFrame<Self> {
        FragmentFrame {
            frame: self,
            limit_size,
            limit_parted,
            parts: HashMap::new(),
            recv_buf: VecDeque::new(),
        }
    }
}

impl<F> Stream for FragmentFrame<F>
where
    F: Stream<Item = Result<(Packet, SocketAddr), CodecError>>,
{
    type Item = Result<(Packet, SocketAddr), CodecError>;

    // TODO
    // Usually, there are a multiple frames frame set packet, and each frame is partitioned. If a
    // frame is not partitioned, then there is only one frame in that frame set. These two kinds of
    // frames are usually not mixed in the same frame set。Verify this hypothesis and optimize
    // the code below.
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        // try empty buffer
        if let Some(buf_res) = this.recv_buf.pop_front() {
            return Poll::Ready(Some(Ok(buf_res)));
        }

        let Some(res) = ready!(this.frame.poll_next(cx)) else {
            return Poll::Ready(None);
        };
        let (packet, addr) = match res {
            Ok((packet, addr)) => (packet, addr),
            Err(err) => {
                return Poll::Ready(Some(Err(err)));
            }
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
                // promise that `parted_index` is always less than `parted_size`
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
                let parts = this.parts.entry(addr).or_default();
                if *this.limit_parted != 0 && parts.len() > *this.limit_parted {
                    return Poll::Ready(Some(Err(CodecError::PartedFrame(format!(
                        "parted_count {} exceed limit_parted {} ",
                        parts.len(),
                        *this.limit_parted,
                    )))));
                }
                let frames_queue = parts.entry(parted_id).or_insert_with(|| {
                    debug!("new parted_id {parted_id} from {addr}, parted_size {parted_size}",);
                    PriorityQueue::with_capacity(parted_size as usize)
                });
                frames_queue.push(frame, Reverse(parted_index));
                if frames_queue.len() < parted_size as usize {
                    continue;
                }
                // `parted_index` is always less than `parted_size`, `frames_queue` length reaches
                // `parted_size`, `frame` is hashed by `parted_index`, so here we get the complete
                // frames vector
                let frames: Vec<Frame> = parts
                    .remove(&parted_id)
                    .unwrap_or_else(|| unreachable!("parted_id {parted_id} should be set before"))
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
        if let Some(buf_res) = this.recv_buf.pop_front() {
            return Poll::Ready(Some(Ok(buf_res)));
        }
        Poll::Pending
    }
}

impl<F> Sink<(Packet, SocketAddr)> for FragmentFrame<F>
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
        let Packet::Connected(connected::Packet::FrameSet(frame_set)) = packet else {
            return this.frame.start_send((packet, addr));
        };
        if matches!(frame_set.inner_pack_id()?, PackId::Disconnect) {
            debug!("disconnect from {}, clean it's frame parts buffer", addr);
            this.parts.remove(&addr);
        }

        todo!()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.poll_flush(cx))?;
        Poll::Ready(Ok(()))
    }
}
