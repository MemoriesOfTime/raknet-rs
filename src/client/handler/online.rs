use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::Bytes;
use futures::{Sink, Stream};

use crate::errors::CodecError;
use crate::packet::connected::FrameBody;

pub(crate) struct OnlineHandler<I, O> {
    read: I,
    write: O,
}

enum State {

}

impl<I, O> Stream for OnlineHandler<I, O>
where
    I: Stream<Item = FrameBody>,
    O: Sink<FrameBody, Error = CodecError>,
{
    type Item = Bytes;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        todo!()
    }
}

impl<I, O> Sink<Bytes> for OnlineHandler<I, O> {
    type Error = crate::errors::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }

    fn start_send(self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        todo!()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        todo!()
    }
}
