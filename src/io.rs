use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::{Sink, Stream};
use minitrace::collector::TraceId;
use pin_project_lite::pin_project;

use crate::errors::Error;
use crate::packet::connected::Reliability;
use crate::Message;

/// Trace info extension for io
pub trait TraceInfo {
    fn last_trace_id(&self) -> Option<TraceId>;
}

/// The basic operation for each connection
pub trait IO:
    Stream<Item = Bytes> + Sink<Bytes, Error = crate::errors::Error> + TraceInfo + Send
{
    fn set_default_reliability(&mut self, reliability: Reliability);
    fn get_default_reliability(&self) -> Reliability;

    fn set_default_order_channel(&mut self, order_channel: u8);
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
    fn set_default_reliability(&mut self, reliability: Reliability) {
        self.default_reliability = reliability;
    }

    fn get_default_reliability(&self) -> Reliability {
        self.default_reliability
    }

    fn set_default_order_channel(&mut self, order_channel: u8) {
        self.default_order_channel = order_channel;
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
