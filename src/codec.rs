mod ack;
mod dedup;
mod fragment;
mod ordered;

use std::borrow::Borrow;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::BytesMut;
use futures::{Sink, Stream};
use pin_project_lite::pin_project;
use tokio::net::UdpSocket;
use tokio_util::codec::{Decoder, Encoder};
use tokio_util::udp::UdpFramed;
use tracing::{trace, warn};

use crate::codec::fragment::DeFragmented;
use crate::errors::CodecError;
use crate::packet::Packet;

/// Codec config
#[derive(Clone, Debug)]
pub(crate) struct CodecConfig {
    /// limit the max size of a parted frames set, 0 means no limit
    /// it will abort the split frame if the parted_size reaches limit.
    limit_size: u32,
    /// limit the max count of all parted frames sets from an address
    /// it might cause client resending frames if the limit is reached.
    limit_parted: usize,
}

impl Default for CodecConfig {
    fn default() -> Self {
        // recommend configuration
        Self {
            limit_size: 256,
            limit_parted: 256,
        }
    }
}

impl CodecConfig {
    pub fn new(limit_size: u32, limit_parted: usize) -> Self {
        Self {
            limit_size,
            limit_parted,
        }
    }
}

pub(crate) trait Framed: Sized {
    fn framed(
        self,
        config: CodecConfig,
    ) -> impl Stream<Item = (Packet, SocketAddr)> + Sink<(Packet, SocketAddr), Error = CodecError>;
}

impl<T: Borrow<UdpSocket> + Sized> Framed for T {
    fn framed(
        self,
        config: CodecConfig,
    ) -> impl Stream<Item = (Packet, SocketAddr)> + Sink<(Packet, SocketAddr), Error = CodecError>
    {
        let frame =
            UdpFramed::new(self, Codec).defragmented(config.limit_size, config.limit_parted);
        LoggedCodec { frame }
    }
}

/// The raknet codec
pub(crate) struct Codec;

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

pin_project! {
    /// Log the error of the packet codec while reading.
    /// We probably don't care about the codec error while decoding request packets.
    struct LoggedCodec<F> {
        #[pin]
        frame: F,
    }
}

impl<F> Stream for LoggedCodec<F>
where
    F: Stream<Item = Result<(Packet, SocketAddr), CodecError>>,
{
    type Item = (Packet, SocketAddr);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            let Some(res) = ready!(this.frame.as_mut().poll_next(cx)) else {
                return Poll::Ready(None);
            };
            let (packet, addr) = match res {
                Ok((packet, addr)) => (packet, addr),
                Err(err) => {
                    warn!("raknet codec error: {err}, ignore this packet");
                    continue;
                }
            };
            trace!("received packet: {packet:?}, from: {addr}",);
            return Poll::Ready(Some((packet, addr)));
        }
    }
}

/// Propagate sink for `LoggedCodec`
impl<F> Sink<(Packet, SocketAddr)> for LoggedCodec<F>
where
    F: Sink<(Packet, SocketAddr)>,
{
    type Error = F::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: (Packet, SocketAddr)) -> Result<(), Self::Error> {
        self.project().frame.start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().frame.poll_close(cx)
    }
}

#[cfg(test)]
mod test {
    use std::net::{SocketAddr, ToSocketAddrs};
    use std::sync::Arc;

    use bytes::Buf;
    use futures::{SinkExt, StreamExt};
    use tokio::net::UdpSocket;
    use tracing::debug;
    use tracing_test::traced_test;

    use crate::codec::{CodecConfig, Framed};
    use crate::packet::{unconnected, Packet};

    fn unconnected_ping() -> Packet {
        Packet::Unconnected(unconnected::Packet::UnconnectedPing {
            send_timestamp: 0,
            magic: true,
            client_guid: 114514,
        })
    }

    #[tokio::test]
    #[traced_test]
    async fn test_truncated_will_not_panic() {
        let socket = Arc::new(
            UdpSocket::bind("0.0.0.0:0".parse::<SocketAddr>().unwrap())
                .await
                .unwrap(),
        );
        let listen_addr = socket.local_addr().unwrap();
        let mut framed = socket.framed(CodecConfig::default()).buffer(10);
        let send_socket = UdpSocket::bind("0.0.0.0:0".parse::<SocketAddr>().unwrap())
            .await
            .unwrap();
        send_socket.send_to(&[1], listen_addr).await.unwrap();
        send_socket
            .framed(CodecConfig::default())
            .send((unconnected_ping(), listen_addr))
            .await
            .unwrap();
        let (packet, _) = framed.next().await.unwrap();
        assert_eq!(packet, unconnected_ping());
    }

    #[tokio::test]
    #[traced_test]
    async fn test_unconnected_ping() {
        let addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
        let socket = Arc::new(UdpSocket::bind(addr).await.unwrap());
        let mut framed = socket.framed(CodecConfig::default()).buffer(10);
        let server_addr = "play.lbsg.net:19132"
            .to_socket_addrs()
            .unwrap()
            .next()
            .unwrap();
        debug!("server address: {}", server_addr);
        framed
            .send((unconnected_ping(), server_addr))
            .await
            .unwrap();
        let (pong, _) = framed.next().await.unwrap();
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
