use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::{Sink, Stream};
use minitrace::collector::TraceId;
use parking_lot::Mutex;
use pin_project_lite::pin_project;

use crate::errors::Error;
use crate::packet::connected::Reliability;
use crate::Message;

/// The basic operation for each connection
pub trait IO:
    Stream<Item = Bytes>
    + Sink<Bytes, Error = crate::errors::Error>
    + Sink<Message, Error = crate::errors::Error>
    + Send
{
    fn set_default_reliability(&mut self, reliability: Reliability);
    fn get_default_reliability(&self) -> Reliability;

    fn set_default_order_channel(&mut self, order_channel: u8);
    fn get_default_order_channel(&self) -> u8;

    /// Get the last `trace_id` after polling Bytes form it, used for end to end tracing
    fn last_trace_id(&self) -> Option<TraceId>;
}

pin_project! {
    /// The detailed implementation of [`crate::io::IO`]
    pub(crate) struct IOImpl<IO> {
        #[pin]
        io: IO,
        default_reliability: Reliability,
        default_order_channel: u8,
        last_trace_id: Option<Arc<Mutex<Option<TraceId>>>>,
    }
}

impl<IO> IOImpl<IO>
where
    IO: Stream<Item = Bytes> + Sink<Message, Error = Error> + Send,
{
    pub(crate) fn new(io: IO) -> Self {
        IOImpl {
            io,
            default_reliability: Reliability::ReliableOrdered,
            default_order_channel: 0,
            last_trace_id: None,
        }
    }

    pub(crate) fn new_with_trace(io: IO, last_trace_id: Arc<Mutex<Option<TraceId>>>) -> Self {
        IOImpl {
            io,
            default_reliability: Reliability::ReliableOrdered,
            default_order_channel: 0,
            last_trace_id: Some(last_trace_id),
        }
    }
}

impl<IO> Stream for IOImpl<IO>
where
    IO: Stream<Item = Bytes>,
{
    type Item = Bytes;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().io.poll_next(cx)
    }
}

impl<IO> Sink<Bytes> for IOImpl<IO>
where
    IO: Sink<Message, Error = Error>,
{
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<Message>::poll_ready(self, cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        let msg = Message::new(self.default_reliability, self.default_order_channel, item);
        Sink::<Message>::start_send(self, msg)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<Message>::poll_flush(self, cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Sink::<Message>::poll_close(self, cx)
    }
}

impl<IO> Sink<Message> for IOImpl<IO>
where
    IO: Sink<Message, Error = Error>,
{
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().io.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        self.project().io.start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().io.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().io.poll_close(cx)
    }
}

impl<IO> crate::io::IO for IOImpl<IO>
where
    IO: Sink<Message, Error = Error> + Stream<Item = Bytes> + Send,
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

    /// Get the last `trace_id` after polling Bytes form it, used for end to end tracing
    fn last_trace_id(&self) -> Option<TraceId> {
        if let Some(ref last_trace_id) = self.last_trace_id {
            return *last_trace_id.lock();
        }
        None
    }
}
