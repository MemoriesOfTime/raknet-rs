use std::pin::Pin;
use std::task::{ready, Context, Poll};

use futures_core::Stream;
use pin_project_lite::pin_project;

pub(crate) trait Logged<T, E>: Sized {
    fn logged_all(
        self,
        ok_f: impl Fn(&T) + Send + Sync + 'static,
        err_f: impl Fn(&E) + Send + Sync + 'static,
    ) -> Log<Self, T, E>;
}

impl<F, T, E> Logged<T, E> for F
where
    F: Stream<Item = Result<T, E>>,
{
    fn logged_all(
        self,
        ok_f: impl Fn(&T) + Send + Sync + 'static,
        err_f: impl Fn(&E) + Send + Sync + 'static,
    ) -> Log<Self, T, E> {
        Log {
            source: self,
            err_f: Box::new(err_f),
            ok_f: Box::new(ok_f),
        }
    }
}

pin_project! {
    pub(crate) struct Log<F, T, E> {
        #[pin]
        source: F,
        err_f: Box<dyn Fn(&E) + Send + Sync>,
        ok_f: Box<dyn Fn(&T) + Send + Sync>,
    }
}

impl<F, T, E> Stream for Log<F, T, E>
where
    F: Stream<Item = Result<T, E>>,
    E: std::error::Error,
{
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            let Some(res) = ready!(this.source.as_mut().poll_next(cx)) else {
                return Poll::Ready(None);
            };
            let v = match res {
                Ok(v) => v,
                Err(ref err) => {
                    (*this.err_f)(err);
                    continue;
                }
            };
            (*this.ok_f)(&v);
            return Poll::Ready(Some(v));
        }
    }
}
