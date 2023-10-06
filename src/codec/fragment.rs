use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use futures::{Sink, Stream};
use pin_project_lite::pin_project;
use priority_queue::PriorityQueue;

use crate::errors::CodecError;
use crate::packet::connected::{Fragment, Frame, Reliability};
use crate::packet::{connected, Packet};

pin_project! {
    /// Fragment/Defragment the frame set packet from sink/stream (UdpFramed). Enable external production
    /// and consumption of continuous frame set packets.
    pub(super) struct FragmentFrame<F> {
        #[pin]
        frame: F,
        reliability: Reliability,
        limit_size: u32,
        parts: HashMap<SocketAddr, HashMap<u16, PriorityQueue<Frame, Reverse<u32>>>>,  // TODO apply LRU to delete old addr
        recv_buf: VecDeque<(Packet, SocketAddr)>,
    }
}

pub(super) trait Fragmented: Sized {
    fn fragmented(self, reliability: Reliability, limit_size: u32) -> FragmentFrame<Self>;
}

impl<T> Fragmented for T
where
    T: Stream<Item = Result<(Packet, SocketAddr), CodecError>>
        + Sink<(Packet, SocketAddr), Error = CodecError>,
{
    fn fragmented(self, reliability: Reliability, limit_size: u32) -> FragmentFrame<Self> {
        FragmentFrame {
            frame: self,
            reliability,
            limit_size,
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
    // frame is not partitioned, then there is only one frame in that frame set. Verify this
    // hypothesis and optimize the code below.
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
        // TODO: with_capacity or new ?
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
                    return Poll::Ready(Some(Err(CodecError::PartedFrameIndexExceed(
                        parted_size - 1,
                        parted_index,
                    ))));
                }
                let parts = this.parts.entry(addr).or_default();
                if *this.limit_size != 0 {
                    if parted_size > *this.limit_size {
                        return Poll::Ready(Some(Err(CodecError::PartedFrameSetSizeExceed(
                            *this.limit_size,
                            parted_size,
                        ))));
                    }
                    if parts.len() > *this.limit_size as usize {
                        return Poll::Ready(Some(Err(CodecError::PartedFrameSetSizeExceed(
                            *this.limit_size,
                            parts.len() as u32,
                        ))));
                    }
                }
                let frames_queue = parts
                    .entry(parted_id)
                    .or_insert_with(|| PriorityQueue::with_capacity(parted_size as usize));
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

impl<F> Sink<()> for FragmentFrame<F> {
    type Error = ();

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }

    fn start_send(self: Pin<&mut Self>, item: ()) -> Result<(), Self::Error> {
        todo!()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }
}
