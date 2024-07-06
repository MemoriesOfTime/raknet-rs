use std::collections::VecDeque;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use futures::Sink;
use log::trace;
use pin_project_lite::pin_project;

use crate::ack::SharedAck;
use crate::errors::CodecError;
use crate::packet::connected::{self, Frame, FrameSet, FramesRef};
use crate::packet::{Packet, FRAME_SET_HEADER_SIZE};
use crate::resend_map::ResendMap;
use crate::utils::u24;
use crate::{PeerContext, RoleContext};

pin_project! {
    // OutgoingGuard equips with Acknowledgement handler and packets buffer and provides
    // resending policies and
    pub(crate) struct OutgoingGuard<F> {
        #[pin]
        frame: F,
        ack: SharedAck,
        seq_num_write_index: u24,
        buf: VecDeque<Frame>,
        peer: PeerContext,
        role: RoleContext,
        cap: usize,
        resend: ResendMap,
    }
}

pub(crate) trait HandleOutgoing: Sized {
    #[allow(clippy::too_many_arguments)]
    fn handle_outgoing(
        self,
        ack: SharedAck,
        cap: usize,
        peer: PeerContext,
        role: RoleContext,
    ) -> OutgoingGuard<Self>;
}

impl<F> HandleOutgoing for F
where
    F: for<'a> Sink<(Packet<FramesRef<'a>>, SocketAddr), Error = CodecError>,
{
    fn handle_outgoing(
        self,
        ack: SharedAck,
        cap: usize,
        peer: PeerContext,
        role: RoleContext,
    ) -> OutgoingGuard<Self> {
        assert!(cap > 0, "cap must larger than 0");
        OutgoingGuard {
            frame: self,
            ack,
            seq_num_write_index: 0.into(),
            buf: VecDeque::with_capacity(cap),
            peer,
            role,
            cap,
            resend: ResendMap::new(),
        }
    }
}

impl<F> OutgoingGuard<F>
where
    F: for<'a> Sink<(Packet<FramesRef<'a>>, SocketAddr), Error = CodecError>,
{
    fn try_empty(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), CodecError>> {
        let mut this = self.project();

        // empty incoming buffer
        this.ack.poll_ack(this.resend);
        this.ack.poll_resend(this.resend, this.buf);
        this.resend.poll_stales_into(this.buf);

        ready!(this.frame.as_mut().poll_ready(cx))?;
        let mut sent = false;

        // TODO: Weighted Round-Robin

        // try empty outgoing buffer
        while !this.ack.empty() || !this.buf.is_empty() {
            // 1st. empty the ack
            if sent {
                ready!(this.frame.as_mut().poll_ready(cx))?;
                sent = false;
            }
            if let Some(ack) = this.ack.poll_outgoing_ack(this.peer.mtu) {
                trace!(
                    "[{}] send ack {ack:?}, total count: {}",
                    this.role,
                    ack.total_cnt()
                );
                this.frame.as_mut().start_send((
                    Packet::Connected(connected::Packet::Ack(ack)),
                    this.peer.addr,
                ))?;
                sent = true;
            }

            // 2nd. empty the nack
            if sent {
                ready!(this.frame.as_mut().poll_ready(cx))?;
                sent = false;
            }
            if let Some(nack) = this.ack.poll_outgoing_nack(this.peer.mtu) {
                trace!(
                    "[{}] send ack {nack:?}, total count: {}",
                    this.role,
                    nack.total_cnt()
                );
                this.frame.as_mut().start_send((
                    Packet::Connected(connected::Packet::Nack(nack)),
                    this.peer.addr,
                ))?;
                sent = true;
            }

            // 3rd. empty the frame_set
            if sent {
                ready!(this.frame.as_mut().poll_ready(cx))?;
                sent = false;
            }

            // TODO: with capacity
            let mut frames = vec![];
            let mut reliable = false;

            // TODO: implement sliding window congestion control to select a proper transmission
            // bandwidth
            let mut remain_mtu = this.peer.mtu as usize - FRAME_SET_HEADER_SIZE;
            while let Some(frame) = this.buf.back() {
                if remain_mtu >= frame.size() {
                    if frame.flags.reliability.is_reliable() {
                        reliable = true;
                    }
                    remain_mtu -= frame.size();
                    frames.push(this.buf.pop_back().unwrap());
                    continue;
                }
                break;
            }
            if !frames.is_empty() {
                let frame_set = FrameSet {
                    seq_num: *this.seq_num_write_index,
                    set: &frames[..],
                };
                this.frame.as_mut().start_send((
                    Packet::Connected(connected::Packet::FrameSet(frame_set)),
                    this.peer.addr,
                ))?;
                sent = true;
                if reliable {
                    // keep for resending
                    this.resend.record(*this.seq_num_write_index, frames);
                }
                *this.seq_num_write_index += 1;
            }
        }

        Poll::Ready(Ok(()))
    }
}

impl<F> Sink<Frame> for OutgoingGuard<F>
where
    F: for<'a> Sink<(Packet<FramesRef<'a>>, SocketAddr), Error = CodecError>,
{
    type Error = CodecError;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let upstream = self.as_mut().try_empty(cx)?;

        if self.buf.len() >= self.cap {
            debug_assert!(
                upstream == Poll::Pending,
                "OutgoingGuard::try_empty returns Ready but buffer still remains!"
            );
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn start_send(self: Pin<&mut Self>, frame: Frame) -> Result<(), Self::Error> {
        let this = self.project();
        this.buf.push_front(frame);
        // Always success
        Ok(())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().try_empty(cx))?;
        debug_assert!(self.buf.is_empty() && self.ack.empty());
        self.project().frame.poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().try_empty(cx))?;
        debug_assert!(self.buf.is_empty() && self.ack.empty());
        self.project().frame.poll_close(cx)
    }
}
