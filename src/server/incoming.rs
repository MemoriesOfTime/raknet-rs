use std::borrow::Borrow;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::Bytes;
use futures::{FutureExt, Sink, SinkExt, Stream, StreamExt};
use tokio::net::UdpSocket;
use tokio_util::udp::UdpFramed;
use tracing::warn;

use super::conn::Conn;
use crate::codec::{Codec, Framed};
use crate::errors::CodecError;
use crate::packet::{unconnected, Packet};
use crate::server::conn::HandeShake;

struct Incoming<F, S> {
    // A `UdpSocket` reference here to create `UdpFramed` for every connection
    //
    // self.socket is always a reference to a `UdpSocket` because self.frame will take
    // T's ownership where T implement `Borrow<UdpSocket>`, that is, we cannot construct
    // a `Incoming` if self.socket is `UdpSocket`. This also means that `Incoming` itself is
    // safely `Unpin`.
    socket: S,
    // A derived version of `UdpFramed`. Noticed that it must be Unpin
    frame: F,
    // Collections of connected addresses, shared among all connections and incoming.
    conns: HashSet<SocketAddr>,
}

impl<F, S> Stream for Incoming<F, S>
where
    F: Stream<Item = Result<(Packet, SocketAddr), CodecError>>
        + Sink<(Packet, SocketAddr), Error = CodecError>
        + Unpin,
    S: Borrow<UdpSocket> + Clone + Unpin,
{
    type Item = Conn<UdpFramed<Codec, S>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Some(res) = ready!(self.frame.poll_next_unpin(cx)) else {
            return Poll::Ready(None);
        };
        let (packet, addr) = match res {
            Ok((packet, addr)) => (packet, addr),
            Err(err) => {
                warn!("poll incoming failed! codec error {err}");
                return Poll::Pending;
            }
        };
        if packet.pack_id().is_unconnected_ping() {
            let pong = Packet::Unconnected(unconnected::Packet::UnconnectedPong {
                send_timestamp: 0,
                server_guid: 0,
                magic: false,
                data: Bytes::new(),
            });
            if let Err(err) = ready!(self.frame.send((pong, addr)).poll_unpin(cx)) {
                warn!("response unconnected pong failed, codec error {err}");
            }
            return Poll::Pending;
        }

        if self.conns.contains(&addr) {
            // TODO  dispatch packet into connected Conn here.
            return Poll::Pending;
        }

        // TODO check packet and open a connection here

        assert!(!self.conns.insert(addr), "cannot open a connection twice");
        let conn = Conn::HandShaking(HandeShake {
            frame: self.socket.clone().framed(),
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
        let frame = socket.framed();
        let incoming = Incoming {
            socket,
            frame,
            conns: HashSet::new(),
        };
        // TODO send connection to incoming
    }
}
