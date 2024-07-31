use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use futures::Sink;
use log::trace;
use pin_project_lite::pin_project;

use crate::link::SharedLink;
use crate::packet::connected::{self, Frame, FrameSet, FramesRef};
use crate::packet::{Packet, FRAME_SET_HEADER_SIZE};
use crate::resend_map::ResendMap;
use crate::utils::u24;
use crate::{Peer, Role};

pin_project! {
    // OutgoingGuard equips with Acknowledgement handler and packets buffer and provides
    // resending policies and
    pub(crate) struct OutgoingGuard<F> {
        #[pin]
        frame: F,
        link: SharedLink,
        seq_num_write_index: u24,
        buf: VecDeque<Frame>,
        peer: Peer,
        role: Role,
        cap: usize,
        resend: ResendMap,
    }
}

pub(crate) trait HandleOutgoing: Sized {
    fn handle_outgoing(
        self,
        link: SharedLink,
        cap: usize,
        peer: Peer,
        role: Role,
    ) -> OutgoingGuard<Self>;
}

impl<F> HandleOutgoing for F
where
    F: for<'a> Sink<(Packet<FramesRef<'a>>, SocketAddr), Error = io::Error>,
{
    fn handle_outgoing(
        self,
        link: SharedLink,
        cap: usize,
        peer: Peer,
        role: Role,
    ) -> OutgoingGuard<Self> {
        assert!(cap > 0, "cap must larger than 0");
        OutgoingGuard {
            frame: self,
            link,
            seq_num_write_index: 0.into(),
            buf: VecDeque::with_capacity(cap),
            peer,
            role,
            cap,
            resend: ResendMap::new(role, peer),
        }
    }
}

impl<F> OutgoingGuard<F>
where
    F: for<'a> Sink<(Packet<FramesRef<'a>>, SocketAddr), Error = io::Error>,
{
    /// Try to empty the outgoing buffer
    fn try_empty(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let mut this = self.project();

        this.link
            .process_ack()
            .for_each(|ack| this.resend.on_ack(ack));
        this.link
            .process_nack()
            .for_each(|nack| this.resend.on_nack_into(nack, this.buf));

        // poll stale frames into buffer
        this.resend.process_stales(this.buf);

        ready!(this.frame.as_mut().poll_ready(cx))?;
        let mut sent = false;

        // TODO: Weighted Round-Robin
        while !this.link.flush_empty() || !this.buf.is_empty() {
            // 1st. empty the nack
            if sent {
                ready!(this.frame.as_mut().poll_ready(cx))?;
                sent = false;
            }
            if let Some(nack) = this.link.process_outgoing_nack(this.peer.mtu) {
                trace!(
                    "[{}] send nack {nack:?} to {}, total count: {}",
                    this.role,
                    this.peer,
                    nack.total_cnt()
                );
                this.frame.as_mut().start_send((
                    Packet::Connected(connected::Packet::Nack(nack)),
                    this.peer.addr,
                ))?;
                sent = true;
            }

            // 2nd. empty the ack
            if sent {
                ready!(this.frame.as_mut().poll_ready(cx))?;
                sent = false;
            }
            if let Some(ack) = this.link.process_outgoing_ack(this.peer.mtu) {
                trace!(
                    "[{}] send ack {ack:?} to {}, total count: {}",
                    this.role,
                    this.peer,
                    ack.total_cnt()
                );
                this.frame.as_mut().start_send((
                    Packet::Connected(connected::Packet::Ack(ack)),
                    this.peer.addr,
                ))?;
                sent = true;
            }

            // 3rd. empty the unconnected packets
            if sent {
                ready!(this.frame.as_mut().poll_ready(cx))?;
                sent = false;
            }
            // only poll one packet each time
            if let Some(packet) = this.link.process_unconnected().next() {
                trace!(
                    "[{}] send unconnected packet to {}, type: {:?}",
                    this.role,
                    this.peer,
                    packet.pack_type()
                );
                this.frame
                    .as_mut()
                    .start_send((Packet::Unconnected(packet), this.peer.addr))?;
                sent = true;
            }

            // 4th. empty the frame set
            if sent {
                ready!(this.frame.as_mut().poll_ready(cx))?;
                sent = false;
            }

            let mut frames = Vec::with_capacity(this.buf.len());
            let mut reliable = false;

            let mut remain = this.peer.mtu as usize - FRAME_SET_HEADER_SIZE;
            while let Some(frame) = this.buf.back() {
                if remain >= frame.size() {
                    if frame.flags.reliability.is_reliable() {
                        reliable = true;
                    }
                    remain -= frame.size();
                    trace!(
                        "[{}] send frame to {}, seq_num: {}, reliable: {}, first byte: 0x{:02x}, size: {}",
                        this.role,
                        this.peer,
                        *this.seq_num_write_index,
                        reliable,
                        frame.body[0],
                        frame.size()
                    );
                    frames.push(this.buf.pop_back().unwrap());
                    continue;
                }
                break;
            }
            debug_assert!(
                this.buf.is_empty() || !frames.is_empty(),
                "every frame size should not exceed MTU"
            );
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
    F: for<'a> Sink<(Packet<FramesRef<'a>>, SocketAddr), Error = io::Error>,
{
    type Error = io::Error;

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
        debug_assert!(self.buf.is_empty() && self.link.flush_empty());
        self.project().frame.poll_flush(cx)
    }

    /// Close the outgoing guard, notice that it may resend infinitely if you do not cancel it.
    /// Insure all frames are received by the peer at the point of closing
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // maybe go to sleep, turn on the waking
        self.link.turn_on_waking();
        loop {
            ready!(self.as_mut().try_empty(cx))?;
            debug_assert!(self.buf.is_empty() && self.link.flush_empty());
            ready!(self.as_mut().project().frame.poll_flush(cx))?;
            if self.resend.is_empty() {
                trace!(
                    "[{}] all frames are received by {}, close the outgoing guard",
                    self.role,
                    self.peer,
                );
                break;
            }
            ready!(self.resend.poll_wait(cx));
        }
        // no need to wake up
        self.link.turn_off_waking();
        self.project().frame.poll_close(cx)
    }
}

// TODO: test
