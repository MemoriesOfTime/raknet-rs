use std::borrow::Cow;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::{Sink, Stream};
use minitrace::collector::SpanContext;
use minitrace::local::{LocalParentGuard, LocalSpan};
use minitrace::Span;
use pin_project_lite::pin_project;

/// Make different spans. Different spans may have different behaviors.
pub(crate) trait MakeSpan {
    fn make(name: impl Into<Cow<'static, str>>) -> Self;

    // Whether to set local parent
    fn try_set_local_parent(&self) -> Option<LocalParentGuard>;
}

// Thread safe span
impl MakeSpan for Span {
    fn make(name: impl Into<Cow<'static, str>>) -> Self {
        Span::root(name, SpanContext::random())
    }

    fn try_set_local_parent(&self) -> Option<LocalParentGuard> {
        Some(self.set_local_parent())
    }
}

// Single thread span
impl MakeSpan for LocalSpan {
    fn make(name: impl Into<Cow<'static, str>>) -> Self {
        LocalSpan::enter_with_local_parent(name)
    }

    fn try_set_local_parent(&self) -> Option<LocalParentGuard> {
        // You must set the local parent before the local span starts
        None
    }
}

#[allow(unused)] // Commutative with SinkExt::enter_on_item
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
    fn enter_on_item<S: MakeSpan, O: Fn(S) -> S>(
        self,
        name: impl Into<Cow<'static, str>>,
        opts: O,
    ) -> EnterOnItem<Self, S, O> {
        EnterOnItem {
            inner: self,
            name: name.into(),
            span: None,
            opts,
        }
    }
}

impl<S: Stream> StreamExt for S {}

#[allow(unused)] // Commutative with StreamExt::enter_on_item
pub(crate) trait SinkExt<I>: Sink<I> + Sized {
    /// It initiates a span each time an operation is performed on this sink, and the span will end
    /// when flushed or closed.
    ///
    /// Tokens in the graph:
    ///   [xxxx]: non-empty sender buffer
    ///   []:     empty sender buffer
    ///
    ///       [-------------**SPAN**--------------]
    ///       v                                   v
    /// [xxxx]             [xxxxxxxxxxxxx]      []
    ///      ^                  ^^^^^^^^^        ^
    ///   ready to send        start send        flushed
    fn enter_on_item<S: MakeSpan, O: Fn(S) -> S>(
        self,
        name: impl Into<Cow<'static, str>>,
        opts: O,
    ) -> EnterOnItem<Self, S, O> {
        EnterOnItem {
            inner: self,
            name: name.into(),
            span: None,
            opts,
        }
    }
}

impl<I, S: Sink<I>> SinkExt<I> for S {}

pin_project! {
    pub(crate) struct EnterOnItem<T, S, O> {
        #[pin]
        inner: T,
        name: Cow<'static, str>,
        span: Option<S>,
        opts: O,
    }
}

impl<T, S, O> Stream for EnterOnItem<T, S, O>
where
    T: Stream,
    S: MakeSpan,
    O: Fn(S) -> S,
{
    type Item = T::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        let span = this
            .span
            .get_or_insert_with(|| (this.opts)(S::make(this.name.clone())));
        let _guard = span.try_set_local_parent();
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

impl<T, I, S, O> Sink<I> for EnterOnItem<T, S, O>
where
    T: Sink<I>,
    S: MakeSpan,
    O: Fn(S) -> S,
{
    type Error = T::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        let span = this
            .span
            .get_or_insert_with(|| (this.opts)(S::make(this.name.clone())));
        let _guard = span.try_set_local_parent();
        let res = this.inner.poll_ready(cx);
        match res {
            r @ Poll::Ready(Err(_)) => {
                // early stopped
                this.span.take();
                r
            }
            other => other,
        }
    }

    fn start_send(self: Pin<&mut Self>, item: I) -> Result<(), Self::Error> {
        let this = self.project();
        let span = this
            .span
            .get_or_insert_with(|| (this.opts)(S::make(this.name.clone())));
        let _guard = span.try_set_local_parent();
        this.inner.start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        let span = this
            .span
            .get_or_insert_with(|| (this.opts)(S::make(this.name.clone())));
        let _guard = span.try_set_local_parent();
        let res = this.inner.poll_flush(cx);
        this.span.take();
        res
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        let span = this
            .span
            .get_or_insert_with(|| (this.opts)(S::make(this.name.clone())));
        let _guard = span.try_set_local_parent();
        let res = this.inner.poll_close(cx);
        this.span.take();
        res
    }
}
