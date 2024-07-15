use std::pin::Pin;
use std::task::{Context, Poll};

use futures::{Sink, Stream};
use minitrace::collector::{SpanContext, TraceId};
use minitrace::Span;
use pin_project_lite::pin_project;

/// Trace info extension for io
pub(crate) trait TraceInfo {
    fn get_last_trace_id(&self) -> Option<TraceId>;
}

pub(crate) trait TraceStreamExt: Stream + Sized {
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
    fn enter_on_item<O: Fn() -> Span>(self, span_fn: O) -> EnterOnItem<Self, O> {
        EnterOnItem {
            inner: self,
            span: None,
            last_trace_id: None,
            span_fn,
        }
    }
}

impl<S: Stream> TraceStreamExt for S {}

pin_project! {
    pub(crate) struct EnterOnItem<T, O> {
        #[pin]
        inner: T,
        span: Option<Span>,
        last_trace_id: Option<TraceId>,
        span_fn: O,
    }
}

impl<T, O> Stream for EnterOnItem<T, O>
where
    T: Stream,
    O: Fn() -> Span,
{
    type Item = T::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        let span = this.span.get_or_insert_with(this.span_fn);
        *this.last_trace_id = SpanContext::from_span(span).map(|ctx| ctx.trace_id);
        let guard = span.set_local_parent(); // set the span as the local thread parent for every poll_next call
        let res = this.inner.poll_next(cx);
        match res {
            r @ Poll::Pending => r, // guard is dropped here before the task is moved
            other => {
                drop(guard);
                // ready for produce a result
                this.span.take();
                other
            }
        }
    }
}

impl<T, O> TraceInfo for EnterOnItem<T, O> {
    fn get_last_trace_id(&self) -> Option<TraceId> {
        self.last_trace_id
    }
}

// TODO: implement ConsoleTreeCollector here

// Propagate Sink trait to inner stream
// TODO: remove sink propagation when IO is splitted
impl<T, I, O> Sink<I> for EnterOnItem<T, O>
where
    T: Sink<I>,
    O: Fn() -> Span,
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
