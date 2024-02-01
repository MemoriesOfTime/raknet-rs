use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::Bytes;
use futures::{Sink, Stream};
use pin_project_lite::pin_project;

use crate::errors::CodecError;
use crate::packet::connected::Frames;
use crate::packet::{unconnected, Packet};

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

pub(crate) struct SortedIterMut<'a, T>
where
    T: Ord,
{
    pq: &'a mut BinaryHeap<T>,
}

impl<'a, T> SortedIterMut<'a, T>
where
    T: Ord,
{
    pub(crate) fn new(pq: &'a mut BinaryHeap<T>) -> Self {
        Self { pq }
    }
}

impl<T> Iterator for SortedIterMut<'_, Reverse<T>>
where
    T: Ord,
{
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.pq.pop().map(|v| v.0)
    }
}

pub(crate) trait WithAddress: Sized {
    fn with_addr(self, addr: SocketAddr) -> WithAddr<Self>;
}

impl<F> WithAddress for F
where
    F: Sink<(Packet<Frames<Bytes>>, SocketAddr), Error = CodecError>,
{
    fn with_addr(self, addr: SocketAddr) -> WithAddr<Self> {
        WithAddr { addr, frame: self }
    }
}

pin_project! {
    pub(crate) struct WithAddr<F> {
        addr: SocketAddr,
        #[pin]
        frame: F,
    }
}

impl<F> Sink<unconnected::Packet> for WithAddr<F>
where
    F: Sink<(Packet<Frames<Bytes>>, SocketAddr), Error = CodecError>,
{
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: unconnected::Packet) -> Result<(), Self::Error> {
        let addr = self.addr;
        self.project()
            .frame
            .start_send((Packet::Unconnected(item), addr))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_close(cx)
    }
}

impl<F> Sink<Packet<Frames<Bytes>>> for WithAddr<F>
where
    F: Sink<(Packet<Frames<Bytes>>, SocketAddr), Error = CodecError>,
{
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Packet<Frames<Bytes>>) -> Result<(), Self::Error> {
        let addr = self.addr;
        self.project().frame.start_send((item, addr))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_close(cx)
    }
}
