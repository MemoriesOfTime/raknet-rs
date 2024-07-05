use std::borrow::Cow;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::{Sink, Stream};
use minitrace::collector::SpanContext;
use minitrace::Span;
use pin_project_lite::pin_project;

pub(crate) trait StreamExt: Stream + Sized {
    /// It starts a span at every time an item is generating from the stream, and the span will end
    /// when an option yield from the stream. So it could be used to track the span from last
    /// reception to the current reception of each packet .
    ///
    ///                [--------------**SPAN**-------------------]
    ///                v                                         v
    /// [---packet1---]           [-----------packet2-----------]
    ///                           ^                             ^
    ///                           [----codec children spans----]
    ///
    /// ------------------------- timeline ------------------------------>>>
    fn enter_on_item<O: Fn(Span) -> Span>(
        self,
        name: impl Into<Cow<'static, str>>,
        opts: O,
    ) -> EnterOnItem<Self, O> {
        EnterOnItem {
            inner: self,
            name: name.into(),
            span: None,
            opts,
        }
    }
}

impl<S: Stream> StreamExt for S {}

pin_project! {
    pub(crate) struct EnterOnItem<T, O> {
        #[pin]
        inner: T,
        name: Cow<'static, str>,
        span: Option<Span>,
        opts: O,
    }
}

impl<T, O> Stream for EnterOnItem<T, O>
where
    T: Stream,
    O: Fn(Span) -> Span,
{
    type Item = T::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        let span = this.span.get_or_insert_with(|| {
            (this.opts)(Span::root(this.name.clone(), SpanContext::random()))
        });
        let _guard = span.set_local_parent();
        let res = this.inner.poll_next(cx);
        match res {
            r @ Poll::Pending => r,
            other => {
                // ready for produce a result
                this.span.take();
                other
            }
        }
    }
}

// TODO: implement ConsoleTreeCollector here

// Propagate Sink trait to inner stream
// TODO: remove sink propagation when IO is splitted
impl<T, I, O> Sink<I> for EnterOnItem<T, O>
where
    T: Sink<I>,
    O: Fn(Span) -> Span,
{
    type Error = T::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().inner.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: I) -> Result<(), Self::Error> {
        self.project().inner.start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().inner.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().inner.poll_close(cx)
    }
}
