use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use futures::{Sink, Stream};
use pin_project_lite::pin_project;

pub(crate) trait Logged<T, E>: Sized {
    fn logged_err(self, f: fn(E)) -> Log<Self, T, E>;

    fn logged_all(self, ok_f: fn(&T), err_f: fn(E)) -> Log<Self, T, E>;
}

impl<F, T, E> Logged<T, E> for F
where
    F: Stream<Item = Result<T, E>>,
{
    fn logged_err(self, f: fn(E)) -> Log<Self, T, E> {
        Log {
            source: self,
            err_f: f,
            ok_f: None,
            _mark: PhantomData,
        }
    }

    fn logged_all(self, ok_f: fn(&T), err_f: fn(E)) -> Log<Self, T, E> {
        Log {
            source: self,
            err_f,
            ok_f: Some(ok_f),
            _mark: PhantomData,
        }
    }
}

pin_project! {
    /// Log the error of the stream while reading.
    pub(crate) struct Log<F, T, E> {
        #[pin]
        source: F,
        err_f: fn(E),
        ok_f: Option<fn(&T)>,
        _mark: PhantomData<T>,
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
                Err(err) => {
                    (*this.err_f)(err);
                    continue;
                }
            };
            if let Some(ok_f) = this.ok_f {
                ok_f(&v);
            }
            return Poll::Ready(Some(v));
        }
    }
}

impl<F, T, E, Si> Sink<Si> for Log<F, T, E>
where
    F: Sink<Si, Error = E>,
{
    type Error = E;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().source.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Si) -> Result<(), Self::Error> {
        self.project().source.start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().source.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().source.poll_close(cx)
    }
}
