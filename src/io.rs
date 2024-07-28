//! The basic operation for each connection

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use fastrace::collector::TraceId;
use futures::{Sink, SinkExt};
use futures_core::Stream;
use pin_project_lite::pin_project;

use crate::errors::Error;
use crate::packet::connected::FrameBody;
use crate::utils::timestamp;
use crate::{Message, Reliability};

// TODO: v0.2.0

/// Reader is a stream of bytes, which can be used to read data from the connection.
#[must_use = "reader do nothing unless polled"]
pub trait Reader: Stream<Item = Bytes> {
    /// Get the remote address of the connection
    fn get_remote_addr(self: Pin<&mut Self>) -> SocketAddr;
}

/// Writer is a sink of bytes, which can be used to write data to the connection.
/// This is a special [`futures::Sink`] with more detailed control.
#[must_use = "writer do nothing unless polled"]
pub trait Writer<Pack> {
    /// Prepare the writer to receive a packet
    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>>;

    /// Send a packet
    ///
    /// Packets encoding is deterministic, and this method will not return an error even when the
    /// internal buffer is full. You need to ensure [`Writer::poll_ready`] to prevent buffer
    /// overflow. (No packets will be discarded when the buffer overflows. So even you use it
    /// incorrectly, it will still work normally.)
    ///
    /// # Panics
    ///
    /// This method will panic when unreasonable parameters are passed in. For example, the
    /// order channel exceeds the maximum value.
    fn send(self: Pin<&mut Self>, pack: Pack);

    /// Flush all buffered packets
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>>;

    /// Ensure all pending packets are delivered and close the connection.
    ///
    /// This method may never return when the peer crashes, an external timeout period needs to be
    /// provided.
    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>>;

    /// Flush the ACK packets buffer.
    ///
    /// This function can be used to implement
    /// [**delayed ack**](https://en.wikipedia.org/wiki/TCP_delayed_acknowledgment) based on timing,
    /// thereby reducing the number of ack packets and improving bandwidth utilization.
    ///
    /// Performing an ACK for a longer period of time may accumulate more sequence numbers, thereby
    /// reducing the number of ACK packets sent. At the same time, sending based on time delay can
    /// avoid deadlocks caused by delaying based on the number of packets. At the same time,
    /// performing an ACK for a shorter period allow the peer to clear the resend map earlier,
    /// reduced the likelihood of retransmission to a certain extent improve performance.
    fn poll_flush_ack(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.poll_flush(cx) // default implementation for some components
    }

    /// Flush the NACK packets buffer.
    ///
    /// Adopting a more aggressive nack flush strategy would be more beneficial for retransmitting
    /// lost packets. At the same time, this may also result in more bandwidth consumption and may
    /// cause the transmission of packets that should not be retransmitted.
    fn poll_flush_nack(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.poll_flush(cx) // default implementation for some components
    }

    /// Flush the user packets buffer.
    ///
    /// Aggressive flush can have lower packets latency, but it will result in lower bandwidth
    /// utilization.
    fn poll_flush_pack(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.poll_flush(cx) // default implementation for some components
    }

    /// Make sure all pending packets are delivered.
    ///
    /// This method may never return when the peer crashes, an external timeout period needs to be
    /// provided.
    fn poll_delivered(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.poll_flush(cx) // default implementation for some components
    }
}

// Extensions

/// Tracing extension for server
pub trait TraceInfo {
    fn last_trace_id(&self) -> Option<TraceId>;
}

/// Ping extension for client, experimental
pub trait Ping {
    fn ping(self: Pin<&mut Self>) -> impl Future<Output = Result<(), Error>> + Send;
}

// TODO: v0.2.0 remove below lines

/// The basic operation for each connection
pub trait IO: Stream<Item = Bytes> + Sink<Bytes, Error = Error> + TraceInfo + Send {
    fn set_default_reliability(self: Pin<&mut Self>, reliability: Reliability);
    fn get_default_reliability(&self) -> Reliability;

    fn set_default_order_channel(self: Pin<&mut Self>, order_channel: u8);
    fn get_default_order_channel(&self) -> u8;

    /// Split into a Stream and a Sink
    fn split(
        self,
    ) -> (
        impl Stream<Item = Bytes> + TraceInfo + Send,
        impl Sink<Message, Error = Error> + Send,
    );
}

pin_project! {
    pub(crate) struct SeparatedIO<I, O> {
        #[pin]
        src: I,
        #[pin]
        dst: O,
        default_reliability: Reliability,
        default_order_channel: u8,
    }
}

impl<I, O> SeparatedIO<I, O>
where
    I: Stream<Item = Bytes> + TraceInfo + Send,
    O: Sink<Message, Error = Error> + Send,
{
    pub(crate) fn new(src: I, dst: O) -> Self {
        SeparatedIO {
            src,
            dst,
            default_reliability: Reliability::ReliableOrdered,
            default_order_channel: 0,
        }
    }
}

impl<I, O> Stream for SeparatedIO<I, O>
where
    I: Stream<Item = Bytes>,
{
    type Item = Bytes;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().src.poll_next(cx)
    }
}

impl<I, O> Sink<Bytes> for SeparatedIO<I, O>
where
    O: Sink<Message, Error = Error>,
{
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().dst.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        let msg = Message::new(self.default_reliability, self.default_order_channel, item);
        self.project().dst.start_send(msg)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().dst.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().dst.poll_close(cx)
    }
}

impl<I, O> TraceInfo for SeparatedIO<I, O>
where
    I: TraceInfo,
{
    fn last_trace_id(&self) -> Option<TraceId> {
        self.src.last_trace_id()
    }
}

impl<I, O> crate::io::IO for SeparatedIO<I, O>
where
    O: Sink<Message, Error = Error> + Send,
    I: Stream<Item = Bytes> + TraceInfo + Send,
{
    fn set_default_reliability(self: Pin<&mut Self>, reliability: Reliability) {
        *self.project().default_reliability = reliability;
    }

    fn get_default_reliability(&self) -> Reliability {
        self.default_reliability
    }

    fn set_default_order_channel(self: Pin<&mut Self>, order_channel: u8) {
        *self.project().default_order_channel = order_channel;
    }

    fn get_default_order_channel(&self) -> u8 {
        self.default_order_channel
    }

    fn split(
        self,
    ) -> (
        impl Stream<Item = Bytes> + TraceInfo + Send,
        impl Sink<Message, Error = Error> + Send,
    ) {
        (self.src, self.dst)
    }
}

impl<I, O> Ping for SeparatedIO<I, O>
where
    O: Sink<Message, Error = Error> + Sink<FrameBody, Error = Error> + Send,
    I: Stream<Item = Bytes> + TraceInfo + Send,
{
    async fn ping(self: Pin<&mut Self>) -> Result<(), Error> {
        self.project()
            .dst
            .send(FrameBody::ConnectedPing {
                client_timestamp: timestamp(),
            })
            .await
    }
}
