use std::borrow::Borrow;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::Bytes;
use futures::{FutureExt, Sink, SinkExt, Stream, StreamExt};
use pin_project_lite::pin_project;
use tokio::net::UdpSocket;
use tracing::warn;

use super::conn::Conn;
use crate::codec::Framed;
use crate::errors::CodecError;
use crate::packet::{unconnected, Packet};
use crate::server::conn::HandeShake;

pin_project! {
    /// A stream of incoming connections.
    struct Incoming<F, S> {
        // A `UdpSocket` reference here to create `UdpFramed` for every connection
        //
        // self.socket is always a reference to a `UdpSocket` because self.frame will take
        // T's ownership where T implement `Borrow<UdpSocket>`, that is, we cannot construct
        // a `Incoming` if self.socket is `UdpSocket`. This also means that `Incoming` itself is
        // safely `Unpin`.
        socket: S,
        // A stream/sink of incoming packets.
        #[pin]
        frame: F,
        // Collections of connected addresses, shared among all connections and incoming.
        conns: HashSet<SocketAddr>,
    }
}

impl<F, S> Stream for Incoming<F, S>
where
    F: Stream<Item = (Packet, SocketAddr)> + Sink<(Packet, SocketAddr), Error = CodecError>,
    S: Borrow<UdpSocket> + Clone + Unpin, // `S` is always Unpin
{
    type Item = Conn<
        impl Stream<Item = (Packet, SocketAddr)> + Sink<(Packet, SocketAddr), Error = CodecError>,
    >;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        let (mut sink, mut stream) = this.frame.split();

        let Some((packet, addr)) = ready!(stream.poll_next_unpin(cx)) else {
            return Poll::Ready(None);
        };
        if packet.pack_id().is_unconnected_ping() {
            let pong = Packet::Unconnected(unconnected::Packet::UnconnectedPong {
                send_timestamp: 0,
                server_guid: 0,
                magic: false,
                data: Bytes::new(),
            });
            if let Err(err) = ready!(sink.send((pong, addr)).poll_unpin(cx)) {
                warn!("response unconnected pong failed, codec error {err}");
            }
            return Poll::Pending;
        }

        if this.conns.contains(&addr) {
            // TODO  dispatch packet into connected Conn here.
            return Poll::Pending;
        }

        // TODO check packet and open a connection here

        assert!(!this.conns.insert(addr), "cannot open a connection twice");
        let frame = this.socket.clone().framed();
        let conn = Conn::HandShaking(HandeShake {
            frame,
            client_protocol_ver: 0,
            client_mtu: 0,
            peer_addr: addr,
        });
        Poll::Ready(Some(conn))
    }
}

#[cfg(test)]
mod test {

    use super::*;

    #[tokio::test]
    async fn test_incoming() {
        let addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
        let socket = &UdpSocket::bind(addr).await.unwrap();
        let frame = socket.framed().split();
        let incoming = Incoming {
            socket,
            frame,
            conns: HashSet::new(),
        };
        // TODO send connection to incoming
    }
}
