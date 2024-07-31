use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use fastrace::collector::TraceId;
use futures::{Future, Sink, SinkExt, Stream};
use pin_project_lite::pin_project;

use crate::packet::connected::FrameBody;
use crate::utils::timestamp;
use crate::{Message, Reliability};

/// Trace info extension for io
pub trait TraceInfo {
    fn last_trace_id(&self) -> Option<TraceId>;
}

/// The basic operation for each connection
pub trait IO: Stream<Item = Bytes> + Sink<Bytes, Error = io::Error> + TraceInfo + Send {
    fn set_default_reliability(self: Pin<&mut Self>, reliability: Reliability);
    fn get_default_reliability(&self) -> Reliability;

    fn set_default_order_channel(self: Pin<&mut Self>, order_channel: u8);
    fn get_default_order_channel(&self) -> u8;

    /// Split into a Stream and a Sink
    fn split(
        self,
    ) -> (
        impl Stream<Item = Bytes> + TraceInfo + Send,
        impl Sink<Message, Error = io::Error> + Send,
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
    O: Sink<Message, Error = io::Error> + Send,
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
    O: Sink<Message, Error = io::Error>,
{
    type Error = io::Error;

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
    O: Sink<Message, Error = io::Error> + Send,
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
        impl Sink<Message, Error = io::Error> + Send,
    ) {
        (self.src, self.dst)
    }
}

/// Ping extension for client, experimental
pub trait Ping {
    fn ping(self: Pin<&mut Self>) -> impl Future<Output = Result<(), io::Error>> + Send;
}

impl<I, O> Ping for SeparatedIO<I, O>
where
    O: Sink<Message, Error = io::Error> + Sink<FrameBody, Error = io::Error> + Send,
    I: Stream<Item = Bytes> + TraceInfo + Send,
{
    async fn ping(self: Pin<&mut Self>) -> Result<(), io::Error> {
        self.project()
            .dst
            .send(FrameBody::ConnectedPing {
                client_timestamp: timestamp(),
            })
            .await
    }
}
