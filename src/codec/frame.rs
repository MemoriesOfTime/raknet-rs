use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::BytesMut;
use fastrace::local::LocalSpan;
use fastrace::{Event, Span};
use futures::{Sink, Stream};
use log::error;

use super::AsyncSocket;
use crate::packet::connected::{FramesMut, FramesRef};
use crate::packet::{unconnected, Packet};

/// `Framed` is a base structure for socket communication.
/// In this project, it wraps an asynchronous UDP socket and implements the
/// [`Stream`](futures::stream::Stream) and [`Sink`](futures::sink::Sink) traits.
/// This allows for reading and writing `RakNet` frames over the socket.
/// It supports both receiving and sending unconnected and connected packets.
pub(crate) struct Framed<T> {
    /// The underlying socket that is being wrapped.
    socket: T,
    /// [maximum transmission unit](https://en.wikipedia.org/wiki/Maximum_transmission_unit),
    /// used to pre-allocate the capacity for both the `rd` and `wr` byte buffers
    max_mtu: usize,
    /// a buffer for storing incoming data read from the socket.
    rd: BytesMut,
    /// a buffer for writing bytes
    wr: BytesMut,
    /// the socket address to which the data will be sent
    out_addr: SocketAddr,
    /// indicates whether the data has been sent to the peer, and the
    /// `wr` buffer has been cleared
    flushed: bool,
    /// indicates whether data has been read from the peer and written
    /// into the `rd` buffer. When `true`, it signifies that the data is ready for frame packet
    /// decoding and will be passed to the upper layer for further processing.
    is_readable: bool,
    /// the address of the current peer
    current_addr: Option<SocketAddr>,
    decode_span: Option<Span>,
    read_span: Option<Span>,
}

impl<T: AsyncSocket> Framed<T> {
    pub(crate) fn new(socket: T, max_mtu: usize) -> Self {
        Self {
            socket,
            max_mtu,
            rd: BytesMut::new(), // we may never use rd at all
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
    fn poll_flush_0(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
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
            ))
        };

        Poll::Ready(res)
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
                pin.is_readable = false;

                // decode one packet at a time
                match Packet::read(&mut pin.rd) {
                    Ok(frame) => {
                        let current_addr = pin
                            .current_addr
                            .expect("will always be set before this line is called");
                        LocalSpan::add_event(Event::new(format!(
                            "{:?} decoded",
                            frame.pack_type()
                        )));
                        pin.decode_span.take();
                        pin.rd.clear();

                        return Poll::Ready(Some((frame, current_addr)));
                    }
                    Err(err) => {
                        error!("failed to decode packet: {}", err);
                        LocalSpan::add_event(Event::new(err.to_string()));
                        pin.decode_span.take();
                        pin.rd.clear();
                    }
                }
            }

            // We're out of data. Try and fetch more data to decode
            pin.read_span
                .get_or_insert_with(|| Span::enter_with_local_parent("codec.frame.read"));
            let addr = match ready!(pin.socket.poll_recv_from(cx, &mut pin.rd)) {
                Ok(addr) => addr,
                Err(err) => {
                    error!("failed to receive data: {:?}", err);
                    LocalSpan::add_event(Event::new(err.to_string()));
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
impl<'a, T: AsyncSocket> Sink<(Packet<FramesRef<'a>>, SocketAddr)> for Framed<T> {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_flush_0(cx)
    }

    fn start_send(
        self: Pin<&mut Self>,
        item: (Packet<FramesRef<'a>>, SocketAddr),
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
        self.poll_flush_0(cx)
    }
}

// Separate the unconnected packets for offline and online states.
// So the offline handler could take account of the unconnected packets and ignore the generic from
// the connected packets.
impl<T: AsyncSocket> Sink<(unconnected::Packet, SocketAddr)> for Framed<T> {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_flush_0(cx)
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
        self.poll_flush_0(cx)
    }
}
