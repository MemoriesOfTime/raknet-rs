mod body;
mod dedup;
mod fragment;
mod ordered;

use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;
use minitrace::Span;
use pin_project_lite::pin_project;

pub(super) use self::body::*;
pub(super) use self::dedup::*;
pub(super) use self::fragment::*;
pub(super) use self::ordered::*;

pin_project! {
    // Trace stream pending span
    pub(super) struct PendingTracer<F> {
        #[pin]
        frame: F,
        span: Option<Span>,
    }
}

pub(super) trait TracePending: Sized {
    fn trace_pending(self) -> PendingTracer<Self>;
}

impl<F> TracePending for F
where
    F: Stream,
{
    fn trace_pending(self) -> PendingTracer<Self> {
        PendingTracer {
            frame: self,
            span: None,
        }
    }
}

impl<F: Stream> Stream for PendingTracer<F> {
    type Item = F::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        match this.frame.poll_next(cx) {
            r @ Poll::Ready(_) => {
                this.span.take();
                r
            }
            p @ Poll::Pending => {
                this.span
                    .get_or_insert_with(|| Span::enter_with_local_parent("codec.pending"));
                p
            }
        }
    }
}
