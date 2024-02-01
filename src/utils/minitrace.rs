use std::borrow::Cow;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::{Sink, Stream};
use minitrace::collector::SpanContext;
use minitrace::local::{LocalParentGuard, LocalSpan};
use minitrace::Span;
use pin_project_lite::pin_project;

impl<T> Instrumented for T {}

/// Make different spans. Different spans may have different behaviors.
pub(crate) trait MakeSpan {
    fn make(name: impl Into<Cow<'static, str>>) -> Self;

    // Whether to set local parent
    fn try_set_local_parent(&self) -> Option<LocalParentGuard>;
}

/// Using root span will collect at the end of each span.
/// For example, when you use [`Instrumented::enter_on_item()`], it will collect once for each
/// processed item.
pub(crate) struct RootSpan(Span);

impl MakeSpan for RootSpan {
    fn make(name: impl Into<Cow<'static, str>>) -> Self {
        RootSpan(Span::root(name, SpanContext::random()))
    }

    // Set local parent for root span
    fn try_set_local_parent(&self) -> Option<LocalParentGuard> {
        Some(self.0.set_local_parent())
    }
}

impl MakeSpan for Span {
    fn make(name: impl Into<Cow<'static, str>>) -> Self {
        Span::enter_with_local_parent(name)
    }

    fn try_set_local_parent(&self) -> Option<LocalParentGuard> {
        None
    }
}

impl MakeSpan for LocalSpan {
    fn make(name: impl Into<Cow<'static, str>>) -> Self {
        LocalSpan::enter_with_local_parent(name)
    }

    fn try_set_local_parent(&self) -> Option<LocalParentGuard> {
        None
    }
}

/// Instrument streams and sinks. The reason for merging them is that it allows some
/// types that implement both stream and sink.
pub(crate) trait Instrumented: Sized {
    /// For [`Stream`]s :
    ///
    /// Binds a [`Span`] to the [`Stream`] that continues to record until the stream is finished.
    ///
    /// Note that for streams that cannot be terminated for a long time, tracking their duration
    /// becomes meaningless. So if you want to measure the time consumed for each item produced by
    /// the stream, please use [`Instrumented::enter_on_item()`].
    ///
    /// For [`Sink`]s :
    ///
    /// Binds a [`Span`] to the [`Sink`] that continues to record until the sink is closed.
    ///
    /// Note that for sinks that cannot be closed for a long time, tracking their duration
    /// becomes meaningless. SO if you want to measure the time consumed for each item sent to the
    /// sink, please use [`Instrumented::enter_on_item()`].
    fn in_span(self, span: Span) -> InSpan<Self> {
        InSpan {
            inner: self,
            span: Some(span),
        }
    }

    /// For [`Stream`]s :
    ///
    /// Starts a span at every [`Stream::poll_next()`]. If the stream gets polled multiple
    /// times, it will create multiple _short_ spans.
    ///
    /// For [`Sink`]s :
    ///
    /// Starts a span at every `Sink::poll_***()`. If the sink gets polled multiple times,
    /// it will create multiple _short_ spans.
    fn enter_on_poll<S: MakeSpan>(
        self,
        name: impl Into<Cow<'static, str>>,
    ) -> EnterOnPoll<Self, S> {
        EnterOnPoll {
            inner: self,
            name: name.into(),
            _phantom: PhantomData,
        }
    }

    /// For [`Stream`]s :
    ///
    /// Like [`Instrumented::enter_on_poll()`], but it starts a span at every time an
    /// item is generating from the stream.
    ///
    /// For [`Sink`]s :
    ///
    /// Like [`Instrumented::enter_on_poll()`], but it starts a span at every time an item
    /// is preparing to send into the sink.
    fn enter_on_item<S: MakeSpan>(
        self,
        name: impl Into<Cow<'static, str>>,
    ) -> EnterOnItem<Self, S> {
        EnterOnItem {
            inner: self,
            name: name.into(),
            span: None,
        }
    }
}

pin_project! {
    pub(crate) struct InSpan<T> {
        #[pin]
        inner: T,
        span: Option<Span>,
    }
}

impl<T> Stream for InSpan<T>
where
    T: Stream,
{
    type Item = T::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        let _guard = this.span.as_ref().map(|s| s.set_local_parent());
        let res = this.inner.poll_next(cx);

        match res {
            r @ Poll::Pending => r,
            r @ Poll::Ready(None) => {
                // finished
                this.span.take();
                r
            }
            other => other,
        }
    }
}

impl<T, I> Sink<I> for InSpan<T>
where
    T: Sink<I>,
{
    type Error = T::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        let _guard = this.span.as_ref().map(|s| s.set_local_parent());
        this.inner.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: I) -> Result<(), Self::Error> {
        let this = self.project();
        let _guard = this.span.as_ref().map(|s| s.set_local_parent());
        this.inner.start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        let _guard = this.span.as_ref().map(|s| s.set_local_parent());
        this.inner.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();

        let _guard = this.span.as_ref().map(|s| s.set_local_parent());
        let res = this.inner.poll_close(cx);

        match res {
            r @ Poll::Pending => r,
            other => {
                // closed
                this.span.take();
                other
            }
        }
    }
}

pin_project! {
    pub(crate) struct EnterOnPoll<T, S: MakeSpan> {
        #[pin]
        inner: T,
        name: Cow<'static, str>,
        _phantom: PhantomData<S>
    }
}

impl<T, S> Stream for EnterOnPoll<T, S>
where
    T: Stream,
    S: MakeSpan,
{
    type Item = T::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        let span = S::make(this.name.clone());
        let _guard = span.try_set_local_parent();
        this.inner.poll_next(cx)
    }
}

impl<T, I, S> Sink<I> for EnterOnPoll<T, S>
where
    T: Sink<I>,
    S: MakeSpan,
{
    type Error = T::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        let span = S::make(this.name.clone());
        let _guard = span.try_set_local_parent();
        this.inner.poll_ready(cx)
    }

    // A weird stuff here that's not called `poll_***` !!!
    fn start_send(self: Pin<&mut Self>, item: I) -> Result<(), Self::Error> {
        let this = self.project();
        let span = S::make(this.name.clone());
        let _guard = span.try_set_local_parent();
        this.inner.start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        let span = S::make(this.name.clone());
        let _guard = span.try_set_local_parent();
        this.inner.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        let span = S::make(this.name.clone());
        let _guard = span.try_set_local_parent();
        this.inner.poll_close(cx)
    }
}

pin_project! {
    pub(crate) struct EnterOnItem<T, S> {
        #[pin]
        inner: T,
        name: Cow<'static, str>,
        span: Option<S>,
    }
}

impl<T, S> Stream for EnterOnItem<T, S>
where
    T: Stream,
    S: MakeSpan,
{
    type Item = T::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        let span = this.span.get_or_insert_with(|| S::make(this.name.clone()));
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

impl<T, I, S> Sink<I> for EnterOnItem<T, S>
where
    T: Sink<I>,
    S: MakeSpan,
{
    type Error = T::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        let span = this.span.get_or_insert_with(|| S::make(this.name.clone()));
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
        debug_assert!(this.span.is_some(), "you must poll_ready before start_send");
        let res = this.inner.start_send(item);
        this.span.take();
        res
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().inner.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().inner.poll_close(cx)
    }
}
