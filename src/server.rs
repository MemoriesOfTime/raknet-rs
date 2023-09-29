use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;

struct Incoming;

struct Conn;

impl Stream for Incoming {
    type Item = Conn;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        todo!()
    }
}
