use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::{Sink, Stream};
use pin_project_lite::pin_project;

use crate::errors::Error;
use crate::packet::connected::Reliability;
use crate::Message;

pin_project! {
    /// The detailed implementation of [`IO`]connections
    pub(crate) struct IOImpl<IO> {
        #[pin]
        io: IO,
        default_reliability: Reliability,
        default_order_channel: u8,
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

impl<IO> crate::IO for IOImpl<IO>
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
}
