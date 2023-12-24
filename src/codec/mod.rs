mod ack;
mod dedup;
mod fragment;
mod ordered;

use std::borrow::Borrow;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::BytesMut;
use derive_builder::Builder;
use futures::{Sink, Stream};
use pin_project_lite::pin_project;
use tokio::net::UdpSocket;
use tokio_util::codec::{Decoder, Encoder};
use tokio_util::udp::UdpFramed;
use tracing::{debug, trace};

use self::ordered::Ordered;
use crate::codec::dedup::Deduplicated;
use crate::codec::fragment::DeFragmented;
use crate::errors::CodecError;
use crate::packet::Packet;

/// Codec config
#[derive(Clone, Debug, Builder)]
pub(crate) struct CodecConfig {
    /// Limit the max size of a parted frames set, 0 means no limit
    /// It will abort the split frame if the parted_size reaches limit.
    /// Enable it to avoid DoS attack.
    /// The maximum number of inflight parted frames is limit_size * limit_parted
    limit_size: u32,
    /// Limit the max count of **all** parted frames sets from an address.
    /// It might cause client resending frames if the limit is reached.
    /// Enable it to avoid DoS attack.
    /// The maximum number of inflight parted frames is limit_size * limit_parted.
    limit_parted: usize,
    /// Maximum ordered channel, the value should be less than 256
    max_channels: usize,
    // Limit the maximum deduplication gap for a connection.
    // Enable it to avoid D-DoS attack based on deduplication.
    max_dedup_gap: usize,
    /// Whether to enable ordered sending when no ordered field found in frame
    default_ordered_send: bool,
}

impl Default for CodecConfig {
    fn default() -> Self {
        // recommend configuration
        Self {
            limit_size: 256,
            limit_parted: 256,
            max_channels: 1,
            max_dedup_gap: 1024,
            default_ordered_send: true,
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
        let frame = UdpFramed::new(self, Codec)
            .deduplicated(config.max_dedup_gap)
            .defragmented(config.limit_size, config.limit_parted)
            .ordered(config.max_channels, config.default_ordered_send);
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
    F: Stream<Item = Result<(Packet, SocketAddr), CodecError>> + Sink<(Packet, SocketAddr)>,
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
                    debug!("raknet codec error: {err}, ignore this packet");
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

trait PollPacket {
    #[allow(clippy::type_complexity)] // not too bad
    fn poll_packet<T>(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Result<(Packet, SocketAddr), Poll<Option<Result<T, CodecError>>>>;
}

impl<F> PollPacket for F
where
    F: Stream<Item = Result<(Packet, SocketAddr), CodecError>>,
{
    fn poll_packet<T>(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Result<(Packet, SocketAddr), Poll<Option<Result<T, CodecError>>>> {
        let res = match self.poll_next(cx) {
            Poll::Ready(res) => res,
            Poll::Pending => return Err(Poll::Pending),
        };
        let Some(res) = res else {
            return Err(Poll::Ready(None));
        };
        res.map_err(|err| Poll::Ready(Some(Err(err))))
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
        // TODO replace with mocked server
        let server_addr = "mc.advancius.net:19132"
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
