use std::collections::{BTreeMap, VecDeque};
use std::ops::Add;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use flume::{Receiver, Sender};
use futures::{Sink, Stream, StreamExt};
use log::trace;
use minstant::Instant;
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::{self, AckOrNack, Frame, FrameSet, Frames, Record};
use crate::packet::{Packet, FRAME_SET_HEADER_SIZE};
use crate::utils::{priority_mpsc, u24};

pin_project! {
    pub(crate) struct IncomingGuard<F> {
        #[pin]
        frame: F,
        ack_tx: Sender<AckOrNack>,
        nack_tx: Sender<AckOrNack>,
    }
}

pub(crate) trait HandleIncoming: Sized {
    fn handle_incoming(
        self,
        ack_tx: Sender<AckOrNack>,
        nack_tx: Sender<AckOrNack>,
    ) -> IncomingGuard<Self>;
}

impl<F, S> HandleIncoming for F
where
    F: Stream<Item = connected::Packet<S>>,
{
    fn handle_incoming(
        self,
        ack_tx: Sender<AckOrNack>,
        nack_tx: Sender<AckOrNack>,
    ) -> IncomingGuard<Self> {
        IncomingGuard {
            frame: self,
            ack_tx,
            nack_tx,
        }
    }
}

impl<F, S> Stream for IncomingGuard<F>
where
    F: Stream<Item = connected::Packet<S>>,
{
    type Item = FrameSet<S>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        loop {
            let Some(packet) = ready!(this.frame.poll_next_unpin(cx)) else {
                return Poll::Ready(None);
            };
            match packet {
                connected::Packet::FrameSet(frame_set) => return Poll::Ready(Some(frame_set)),
                connected::Packet::Ack(ack) => {
                    this.ack_tx.send(ack).expect("ack_rx must not be dropped");
                }
                connected::Packet::Nack(nack) => {
                    this.nack_tx.send(nack).expect("ack_rx must not be dropped");
                }
            };
        }
    }
}

#[derive(Debug)]
pub(crate) enum Peer {
    Client,
    Server,
}

impl std::fmt::Display for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Peer::Client => write!(f, "client"),
            Peer::Server => write!(f, "server"),
        }
    }
}

pin_project! {
    pub(crate) struct OutgoingGuard<F> {
        #[pin]
        frame: F,
        incoming_ack_rx: Receiver<AckOrNack>,
        incoming_nack_rx: Receiver<AckOrNack>,
        outgoing_ack_rx: priority_mpsc::Receiver<u24>,
        outgoing_nack_rx: priority_mpsc::Receiver<u24>,
        seq_num_write_index: u24,
        buf: VecDeque<Frame<Bytes>>,
        peer: Peer,
        cap: usize,
        mtu: u16,
        // ordered by seq_num
        // TODO: use rbtree?
        resending: BTreeMap<u24, (Frames<Bytes>, Instant)>,
    }
}

pub(crate) trait HandleOutgoingAck: Sized {
    #[allow(clippy::too_many_arguments)]
    fn handle_outgoing(
        self,
        incoming_ack_rx: Receiver<AckOrNack>,
        incoming_nack_rx: Receiver<AckOrNack>,
        outgoing_ack_rx: priority_mpsc::Receiver<u24>,
        outgoing_nack_rx: priority_mpsc::Receiver<u24>,
        cap: usize,
        mtu: u16,
        peer: Peer,
    ) -> OutgoingGuard<Self>;
}

impl<F> HandleOutgoingAck for F
where
    F: Sink<Packet<Frames<Bytes>>, Error = CodecError>,
{
    fn handle_outgoing(
        self,
        incoming_ack_rx: Receiver<AckOrNack>,
        incoming_nack_rx: Receiver<AckOrNack>,
        outgoing_ack_rx: priority_mpsc::Receiver<u24>,
        outgoing_nack_rx: priority_mpsc::Receiver<u24>,
        cap: usize,
        mtu: u16,
        peer: Peer,
    ) -> OutgoingGuard<Self> {
        assert!(cap > 0, "cap must larger than 0");
        OutgoingGuard {
            frame: self,
            incoming_ack_rx,
            incoming_nack_rx,
            outgoing_ack_rx,
            outgoing_nack_rx,
            seq_num_write_index: 0.into(),
            buf: VecDeque::with_capacity(cap),
            peer,
            cap,
            mtu,
            resending: BTreeMap::new(),
        }
    }
}

// TODO: use adaptive RTO
const RTO: Duration = Duration::from_millis(77);

impl<F> OutgoingGuard<F>
where
    F: Sink<Packet<Frames<Bytes>>, Error = CodecError>,
{
    fn try_ack(self: Pin<&mut Self>) {
        let this = self.project();
        for ack in this.incoming_ack_rx.try_iter() {
            trace!(
                "[{}] receive ack {ack:?}, total count: {}",
                this.peer,
                ack.total_cnt()
            );
            for record in ack.records {
                match record {
                    Record::Range(start, end) => {
                        for i in start.to_u32()..end.to_u32() {
                            // TODO: optimized for range remove for btree map
                            this.resending.remove(&i.into());
                        }
                    }
                    Record::Single(seq_num) => {
                        this.resending.remove(&seq_num);
                    }
                }
            }
        }
        for nack in this.incoming_nack_rx.try_iter() {
            trace!(
                "[{}] receive nack {nack:?}, total count: {}",
                this.peer,
                nack.total_cnt()
            );
            for record in nack.records {
                match record {
                    Record::Range(start, end) => {
                        for i in start.to_u32()..end.to_u32() {
                            if let Some((set, _)) = this.resending.remove(&i.into()) {
                                this.buf.extend(set);
                            }
                        }
                    }
                    Record::Single(seq_num) => {
                        if let Some((set, _)) = this.resending.remove(&seq_num) {
                            this.buf.extend(set);
                        }
                    }
                }
            }
        }
    }

    fn try_send_stales(self: Pin<&mut Self>) {
        let this = self.project();

        let now = Instant::now();
        while let Some(entry) = this.resending.first_entry() {
            // ordered by seq_num, the large seq_num has the large next_send
            // TODO: is it a good optimization?
            if now < entry.get().1 {
                break;
            }
            trace!("[{}] resend stale frame set {}", this.peer, entry.key());
            let (set, _) = entry.remove();
            this.buf.extend(set);
        }
    }

    fn try_empty(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), CodecError>> {
        // try to empty *_ack_rx and *_nack_rx buffer
        self.as_mut().try_ack();
        self.as_mut().try_send_stales();

        let mut this = self.project();

        ready!(this.frame.as_mut().poll_ready(cx))?;

        let mut sent = false;

        while !this.outgoing_ack_rx.is_empty()
            || !this.outgoing_nack_rx.is_empty()
            || !this.buf.is_empty()
        {
            // Round-Robin

            // 1st. sent the ack
            if sent {
                ready!(this.frame.as_mut().poll_ready(cx))?;
                sent = false;
            }
            if let Some(ack) = AckOrNack::extend_from(this.outgoing_ack_rx.recv_batch(), *this.mtu)
            {
                trace!(
                    "[{}] send ack {ack:?}, total count: {}",
                    this.peer,
                    ack.total_cnt()
                );
                this.frame
                    .as_mut()
                    .start_send(Packet::Connected(connected::Packet::Ack(ack)))?;
                sent = true;
            }

            // 2nd. sent the nack
            if sent {
                ready!(this.frame.as_mut().poll_ready(cx))?;
                sent = false;
            }
            if let Some(nack) =
                AckOrNack::extend_from(this.outgoing_nack_rx.recv_batch(), *this.mtu)
            {
                trace!(
                    "[{}] send ack {nack:?}, total count: {}",
                    this.peer,
                    nack.total_cnt()
                );
                this.frame
                    .as_mut()
                    .start_send(Packet::Connected(connected::Packet::Nack(nack)))?;
                sent = true;
            }

            // 3rd. sent the frame_set
            if sent {
                ready!(this.frame.as_mut().poll_ready(cx))?;
                sent = false;
            }

            let mut frames = vec![];
            let mut reliable = false;

            // TODO: implement sliding window congestion control to select a proper transmission
            // bandwidth
            let mut remain_mtu = *this.mtu as usize - FRAME_SET_HEADER_SIZE;
            while let Some(frame) = this.buf.front() {
                if remain_mtu >= frame.size() {
                    if frame.flags.reliability.is_reliable() {
                        reliable = true;
                    }
                    remain_mtu -= frame.size();
                    frames.push(this.buf.pop_front().unwrap());
                    continue;
                }
                break;
            }
            if !frames.is_empty() {
                let frame_set = FrameSet {
                    seq_num: *this.seq_num_write_index,
                    set: frames,
                };
                this.frame
                    .as_mut()
                    .start_send(Packet::Connected(connected::Packet::FrameSet(
                        frame_set.clone(),
                    )))?;
                sent = true;
                if reliable {
                    // keep for resending
                    this.resending
                        .insert(frame_set.seq_num, (frame_set.set, Instant::now().add(RTO)));
                }
                *this.seq_num_write_index += 1;
            }
        }

        Poll::Ready(Ok(()))
    }
}

impl<F> Sink<Frames<Bytes>> for OutgoingGuard<F>
where
    F: Sink<Packet<Frames<Bytes>>, Error = CodecError>,
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

    fn start_send(self: Pin<&mut Self>, frames: Frames<Bytes>) -> Result<(), Self::Error> {
        let this = self.project();
        this.buf.extend(frames);
        Ok(())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().try_empty(cx))?;
        debug_assert!(
            self.buf.is_empty()
                && self.outgoing_ack_rx.is_empty()
                && self.outgoing_nack_rx.is_empty()
        );
        self.project().frame.poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().try_empty(cx))?;
        debug_assert!(
            self.buf.is_empty()
                && self.outgoing_ack_rx.is_empty()
                && self.outgoing_nack_rx.is_empty()
        );
        self.project().frame.poll_close(cx)
    }
}

impl<F> Sink<Frame<Bytes>> for OutgoingGuard<F>
where
    F: Sink<Packet<Frames<Bytes>>, Error = CodecError>,
{
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<Frames<Bytes>>::poll_ready(self, cx)
    }

    fn start_send(self: Pin<&mut Self>, frame: Frame<Bytes>) -> Result<(), Self::Error> {
        let this = self.project();
        this.buf.push_back(frame); // optimizing for non-split frame
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<Frames<Bytes>>::poll_flush(self, cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<Frames<Bytes>>::poll_close(self, cx)
    }
}
