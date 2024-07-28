//! The basic operation for each connection

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use fastrace::collector::TraceId;
use futures_core::Stream;
use pin_project_lite::pin_project;

use crate::errors::Error;

// TODO: v0.2.0

/// Reader is a stream of bytes, which can be used to read data from the connection.
#[must_use = "reader do nothing unless polled"]
pub trait Reader: Stream<Item = Bytes> {
    /// Get the remote address of the connection
    fn get_remote_addr(&self) -> SocketAddr;
}

/// Writer is a sink of bytes, which can be used to write data to the connection.
/// This is a special [`futures::Sink`] with more detailed control.
#[must_use = "writer do nothing unless polled"]
pub trait Writer<Pack> {
    /// Prepare the writer to receive a packet
    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>>;

    /// Feed a packet
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
    /// This method will also panic when the connection is closed.
    fn feed(self: Pin<&mut Self>, pack: Pack);

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

pin_project! {
    // A wrapper for the reader to add the remote address
    pub(crate) struct ReaderWrapper<T> {
        #[pin]
        reader: T,
        remote_addr: SocketAddr,
    }
}

impl<T> ReaderWrapper<T> {
    pub(crate) fn new(reader: T, remote_addr: SocketAddr) -> Self {
        Self {
            reader,
            remote_addr,
        }
    }
}

impl<T> Stream for ReaderWrapper<T>
where
    T: Stream<Item = Bytes>,
{
    type Item = Bytes;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().reader.poll_next(cx)
    }
}

impl<T> Reader for ReaderWrapper<T>
where
    T: Stream<Item = Bytes>,
{
    fn get_remote_addr(&self) -> SocketAddr {
        self.remote_addr
    }
}

impl<R: TraceInfo> TraceInfo for ReaderWrapper<R> {
    fn last_trace_id(&self) -> Option<TraceId> {
        self.reader.last_trace_id()
    }
}
