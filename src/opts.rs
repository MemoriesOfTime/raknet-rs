use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use fastrace::collector::TraceId;
use futures::{Sink, SinkExt};
use pin_project_lite::pin_project;

use crate::link::SharedLink;
use crate::packet::connected::{Frame, FrameBody};
use crate::utils::timestamp;
use crate::{Message, Peer};

/// Trace info extension for server
pub trait TraceInfo {
    fn last_trace_id(&self) -> Option<TraceId>;
}

/// Obtain the connection information
pub trait ConnectionInfo {
    fn mtu(&self) -> u16;
    fn remote_addr(&self) -> SocketAddr;
    fn guid(&self) -> u64;
}

pub(crate) trait WrapConnectionInfo: Sized {
    fn wrap_connection_info(self, peer: Peer) -> ConnectionInfoWrapper<Self>;
}

pin_project! {
    pub(crate) struct ConnectionInfoWrapper<I> {
        #[pin]
        inner: I,
        peer: Peer,
    }
}

impl<I> ConnectionInfo for ConnectionInfoWrapper<I> {
    fn mtu(&self) -> u16 {
        self.peer.mtu
    }

    fn remote_addr(&self) -> SocketAddr {
        self.peer.addr
    }

    fn guid(&self) -> u64 {
        self.peer.guid
    }
}

impl<T, I: Sink<T>> Sink<T> for ConnectionInfoWrapper<I> {
    type Error = I::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().inner.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: T) -> Result<(), Self::Error> {
        self.project().inner.start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().inner.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().inner.poll_close(cx)
    }
}

impl<S: Sink<Message>> WrapConnectionInfo for S {
    fn wrap_connection_info(self, peer: Peer) -> ConnectionInfoWrapper<Self> {
        ConnectionInfoWrapper { inner: self, peer }
    }
}

/// Ping extension for client, experimental
pub trait Ping {
    fn ping(self: Pin<&mut Self>) -> impl Future<Output = Result<(), io::Error>> + Send;
}

impl<S> Ping for S
where
    S: Sink<FrameBody, Error = io::Error> + Send,
{
    async fn ping(mut self: Pin<&mut Self>) -> Result<(), io::Error> {
        self.send(FrameBody::ConnectedPing {
            client_timestamp: timestamp(),
        })
        .await
    }
}

/// Flush strategy can be used as ext data of [`std::task::Context`] to guide how
/// [`Sink::poll_flush`] perform flush. And the results after flush will be stored here.
/// The default strategy will flush all buffers.
///
/// Customizing your own strategy can achieve many features:
///
/// 1. [**Delayed ack**](https://en.wikipedia.org/wiki/TCP_delayed_acknowledgment) based on timing,
/// thereby reducing the number of ack packets and improving bandwidth utilization. At the same
/// time, sending based on timing can avoid deadlocks or regressions caused by delaying based on the
/// number of packets.
///
/// 2. More aggressive nack/pack flush strategy which would be more beneficial for retransmitting
/// packets.
///
/// After the flush is completed, the strategy will store the number of frames that have been
/// flushed. You can use this number to determine when to take the next flush.
///
/// Note that it can only be used in [`Sink::poll_flush`].
#[derive(Debug, Default, Clone, Copy)]
pub struct FlushStrategy {
    ack_tag: isize,
    nack_tag: isize,
    pack_tag: isize,
}

impl FlushStrategy {
    /// Create a new flush strategy with specified flush options.
    pub fn new(ack: bool, nack: bool, pack: bool) -> Self {
        FlushStrategy {
            ack_tag: if ack { 0 } else { -1 },
            nack_tag: if nack { 0 } else { -1 },
            pack_tag: if pack { 0 } else { -1 },
        }
    }

    /// Get how many ack frames have been flushed.
    ///
    /// # Panics
    /// It will panic if ack flush is not enabled.
    pub fn flushed_ack(&self) -> usize {
        assert!(
            self.ack_tag != -1,
            "you should enable flush ack before checking result of flushed ack"
        );
        self.ack_tag as usize
    }

    /// Get how many nack frames have been flushed.
    ///
    /// # Panics
    /// It will panic if nack flush is not enabled.
    pub fn flushed_nack(&self) -> usize {
        assert!(
            self.nack_tag != -1,
            "you should enable flush nack before checking result of flushed nack"
        );
        self.nack_tag as usize
    }

    /// Get how many pack frames have been flushed.
    ///
    /// # Panics
    /// It will panic if pack flush is not enabled.
    pub fn flushed_pack(&self) -> usize {
        assert!(
            self.pack_tag != -1,
            "you should enable flush pack before checking result of flushed pack"
        );
        self.pack_tag as usize
    }

    pub(crate) fn check_flushed(&self, link: &SharedLink, buf: &VecDeque<Frame>) -> bool {
        let mut ret = true;
        if self.ack_tag != -1 {
            ret &= link.outgoing_ack_empty();
        }
        if self.nack_tag != -1 {
            ret &= link.outgoing_nack_empty();
        }
        if self.pack_tag != -1 {
            ret &= link.unconnected_empty() && buf.is_empty();
        }
        ret
    }

    pub(crate) fn flush_ack(&self) -> bool {
        self.ack_tag != -1
    }

    pub(crate) fn flush_nack(&self) -> bool {
        self.nack_tag != -1
    }

    pub(crate) fn flush_pack(&self) -> bool {
        self.pack_tag != -1
    }

    pub(crate) fn mark_flushed_ack(&mut self, cnt: usize) {
        if self.ack_tag == -1 {
            return;
        }
        self.ack_tag += cnt as isize;
    }

    pub(crate) fn mark_flushed_nack(&mut self, cnt: usize) {
        if self.nack_tag == -1 {
            return;
        }
        self.nack_tag += cnt as isize;
    }

    pub(crate) fn mark_flushed_pack(&mut self, cnt: usize) {
        if self.pack_tag == -1 {
            return;
        }
        self.pack_tag += cnt as isize;
    }
}
