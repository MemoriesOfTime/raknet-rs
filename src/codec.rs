use std::borrow::Borrow;

use bytes::BytesMut;
use tokio::net::UdpSocket;
use tokio_util::codec::{Decoder, Encoder};
use tokio_util::udp::UdpFramed;

use crate::errors::CodecError;
use crate::packet::Packet;

/// The raknet codec
pub(crate) struct Codec;

pub(crate) trait Framed: Sized {
    fn framed(self) -> UdpFramed<Codec, Self>;
}

impl<T: Borrow<UdpSocket> + Sized> Framed for T {
    fn framed(self) -> UdpFramed<Codec, Self> {
        UdpFramed::new(self, Codec)
    }
}

impl Encoder<Packet> for Codec {
    type Error = CodecError;

    fn encode(&mut self, item: Packet, dst: &mut BytesMut) -> Result<(), Self::Error> {
        item.write(dst);
        Ok(())
    }
}

impl Decoder for Codec {
    type Error = CodecError;
    type Item = Packet;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        Packet::read(src)
    }
}

#[cfg(test)]
mod test {
    use std::net::{SocketAddr, ToSocketAddrs};
    use std::sync::Arc;

    use bytes::Buf;
    use futures::{SinkExt, StreamExt};
    use tokio::net::UdpSocket;

    use crate::codec::Framed;
    use crate::packet::{unconnected, Packet};

    #[tokio::test]
    async fn test_unconnected_ping() {
        let addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
        let socket = Arc::new(UdpSocket::bind(addr).await.unwrap());
        let mut framed = socket.framed().buffer(10);
        let packet = unconnected::Packet::UnconnectedPing {
            send_timestamp: 0,
            magic: true,
            client_guid: 114514,
        };
        let server_addr = "play.lbsg.net:19132"
            .to_socket_addrs()
            .unwrap()
            .next()
            .unwrap();
        framed
            .send((Packet::Unconnected(packet), server_addr))
            .await
            .unwrap();
        let (pong, _) = framed.next().await.unwrap().unwrap();
        assert!(matches!(
            pong,
            Packet::Unconnected(unconnected::Packet::UnconnectedPong { .. })
        ));
        if let Packet::Unconnected(unconnected::Packet::UnconnectedPong {
            send_timestamp,
            server_guid: _,
            magic,
            data,
        }) = pong
        {
            assert_eq!(send_timestamp, 0);
            assert!(magic);
            let motd = String::from_utf8_lossy(data.chunk());
            assert!(motd.contains("MCPE"));
        }
    }
}
