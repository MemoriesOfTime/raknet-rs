use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::{Buf, BytesMut};
use futures::{Sink, Stream};
use log::error;
use fastrace::{Event, Span};

use super::AsyncSocket;
use crate::errors::CodecError;
use crate::packet::connected::{FramesMut, FramesRef};
use crate::packet::{unconnected, Packet};

pub(crate) struct Framed<T> {
    socket: T,
    max_mtu: usize,
    rd: BytesMut,
    wr: BytesMut,
    out_addr: SocketAddr,
    flushed: bool,
    is_readable: bool,
    current_addr: Option<SocketAddr>,
    decode_span: Option<Span>,
    read_span: Option<Span>,
}

impl<T: AsyncSocket> Framed<T> {
    pub(crate) fn new(socket: T, max_mtu: usize) -> Self {
        Self {
            socket,
            max_mtu,
            rd: BytesMut::with_capacity(max_mtu),
            wr: BytesMut::with_capacity(max_mtu),
            out_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
            flushed: true,
            is_readable: false,
            current_addr: None,
            decode_span: None,
            read_span: None,
        }
    }

    #[inline]
    fn poll_ready_0(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), CodecError>> {
        if !self.flushed {
            match self.poll_flush_0(cx)? {
                Poll::Ready(()) => {}
                Poll::Pending => return Poll::Pending,
            }
        }

        Poll::Ready(Ok(()))
    }

    #[inline]
    fn poll_flush_0(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), CodecError>> {
        if self.flushed {
            return Poll::Ready(Ok(()));
        }

        let Self {
            ref socket,
            ref mut out_addr,
            ref mut wr,
            ..
        } = *self;

        let n = ready!(socket.poll_send_to(cx, wr, *out_addr))?;

        let wrote_all = n == self.wr.len();
        self.wr.clear();
        self.flushed = true;

        let res = if wrote_all {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::Other,
                "failed to write entire datagram to socket",
            )
            .into())
        };

        Poll::Ready(res)
    }

    #[inline]
    fn poll_close_0(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), CodecError>> {
        ready!(self.poll_flush_0(cx))?;
        Poll::Ready(Ok(()))
    }
}

impl<T: AsyncSocket> Stream for Framed<T> {
    type Item = (Packet<FramesMut>, SocketAddr);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let pin = self.get_mut();

        pin.rd.reserve(pin.max_mtu);

        loop {
            // Are there still bytes left in the read buffer to decode?
            if pin.is_readable {
                match Packet::read(&mut pin.rd) {
                    Ok(Some(frame)) => {
                        let current_addr = pin
                            .current_addr
                            .expect("will always be set before this line is called");
                        Event::add_to_local_parent(
                            format!("{:?} decoded", frame.pack_type()),
                            || [],
                        );
                        return Poll::Ready(Some((frame, current_addr)));
                    }
                    Err(err) => {
                        Event::add_to_local_parent(err.to_string(), || []);
                        error!("failed to decode packet: {:?}", err);
                    }
                    Ok(None) => {}
                }
                // if this line has been reached then decode has returned `None` or an error

                pin.decode_span.take(); // finish the decode span
                pin.is_readable = false;
                pin.rd.clear();
            }

            // We're out of data. Try and fetch more data to decode
            pin.read_span
                .get_or_insert_with(|| Span::enter_with_local_parent("codec.frame.read"));
            let addr = match ready!(pin.socket.poll_recv_from(cx, &mut pin.rd)) {
                Ok(addr) => addr,
                Err(err) => {
                    error!("failed to receive data: {:?}", err);
                    Event::add_to_local_parent(err.to_string(), || []);
                    pin.rd.clear();
                    continue;
                }
            };
            // finish the read span
            pin.read_span.take();
            // start a new decode span
            pin.decode_span.get_or_insert_with(|| {
                Span::enter_with_local_parent("codec.frame.decode").with_properties(|| {
                    [
                        ("addr", addr.to_string()),
                        ("datagram_size", pin.rd.len().to_string()),
                    ]
                })
            });
            pin.current_addr = Some(addr);
            pin.is_readable = true;
        }
    }
}

/// The `Sink` implementation for cheap buffer cloning (i.e. `bytes::Bytes`).
impl<'a, B: Buf + Clone, T: AsyncSocket> Sink<(Packet<FramesRef<'a, B>>, SocketAddr)>
    for Framed<T>
{
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_ready_0(cx)
    }

    fn start_send(
        self: Pin<&mut Self>,
        item: (Packet<FramesRef<'a, B>>, SocketAddr),
    ) -> Result<(), Self::Error> {
        let (frame, out_addr) = item;

        let pin = self.get_mut();

        frame.write(&mut pin.wr);
        pin.out_addr = out_addr;
        pin.flushed = false;

        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_flush_0(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_close_0(cx)
    }
}

// Separate the unconnected packets for offline and online states.
// So the offline handler could take account of the unconnected packets and ignore the generic from
// the connected packets.
impl<T: AsyncSocket> Sink<(unconnected::Packet, SocketAddr)> for Framed<T> {
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_ready_0(cx)
    }

    fn start_send(
        self: Pin<&mut Self>,
        item: (unconnected::Packet, SocketAddr),
    ) -> Result<(), Self::Error> {
        let (frame, out_addr) = item;

        let pin = self.get_mut();

        frame.write(&mut pin.wr);
        pin.out_addr = out_addr;
        pin.flushed = false;

        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_flush_0(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_close_0(cx)
    }
}
