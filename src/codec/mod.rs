mod dedup;
mod fragment;
mod frame;
mod ordered;

use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Buf, Bytes, BytesMut};
use derive_builder::Builder;
use futures::{ready, Stream};
use pin_project_lite::pin_project;
use tokio_util::codec::{Decoder, Encoder};
use tracing::{debug, trace};

use self::frame::FrameDecoded;
use self::ordered::Ordered;
use crate::codec::dedup::Deduplicated;
use crate::codec::fragment::DeFragmented;
use crate::errors::CodecError;
use crate::packet::connected::FrameBody;
use crate::packet::{connected, Packet};

/// Codec config
#[derive(Clone, Copy, Debug, Builder)]
pub struct CodecConfig {
    /// Limit the max size of a parted frames set, 0 means no limit
    /// It will abort the split frame if the parted_size reaches limit.
    /// Enable it to avoid DoS attack.
    /// The maximum number of inflight parted frames is max_parted_size * max_parted_count
    max_parted_size: u32,
    /// Limit the max count of **all** parted frames sets from an address.
    /// It might cause client resending frames if the limit is reached.
    /// Enable it to avoid DoS attack.
    /// The maximum number of inflight parted frames is max_parted_size * max_parted_count
    max_parted_count: usize,
    /// Maximum ordered channel, the value should be less than 256
    max_channels: usize,
    // Limit the maximum deduplication gap for a connection, 0 means no limit.
    // Enable it to avoid D-DoS attack based on deduplication.
    max_dedup_gap: usize,
}

impl Default for CodecConfig {
    fn default() -> Self {
        // recommend configuration
        Self {
            max_parted_size: 256,
            max_parted_count: 256,
            max_channels: 1,
            max_dedup_gap: 1024,
        }
    }
}

pub(crate) trait Decoded {
    fn decoded(
        self,
        addr: SocketAddr,
        config: CodecConfig,
    ) -> impl Stream<Item = connected::Packet<FrameBody>>;
}

impl<F> Decoded for F
where
    F: Stream<Item = Result<connected::Packet<BytesMut>, CodecError>>,
{
    fn decoded(
        self,
        addr: SocketAddr,
        config: CodecConfig,
    ) -> impl Stream<Item = connected::Packet<FrameBody>> {
        self.deduplicated(config.max_dedup_gap)
            .defragmented(config.max_parted_size, config.max_parted_count)
            .ordered(config.max_channels)
            .frame_decoded()
            .logged(addr)
    }
}

pin_project! {
    /// Log the error of the packet codec while reading.
    /// We probably don't care about the codec error while decoding request packets.
    struct Log<F> {
        #[pin]
        frame: F,
        addr: SocketAddr,
    }
}

pub(crate) trait Logged: Sized {
    fn logged(self, addr: SocketAddr) -> Log<Self>;
}

impl<F> Logged for F {
    fn logged(self, addr: SocketAddr) -> Log<Self> {
        Log { frame: self, addr }
    }
}

impl<F, B> Stream for Log<F>
where
    F: Stream<Item = Result<connected::Packet<B>, CodecError>>,
{
    type Item = connected::Packet<B>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            let Some(res) = ready!(this.frame.as_mut().poll_next(cx)) else {
                return Poll::Ready(None);
            };
            let packet = match res {
                Ok(packet) => packet,
                Err(err) => {
                    debug!(
                        "raknet codec error: {err} from {}, ignore this packet",
                        this.addr
                    );
                    continue;
                }
            };
            trace!(
                "received packet: {:?} from {}",
                packet.pack_type(),
                this.addr
            );
            return Poll::Ready(Some(packet));
        }
    }
}

/// The raknet codec
pub(crate) struct Codec;

impl<B: Buf> Encoder<Packet<B>> for Codec {
    type Error = CodecError;

    fn encode(&mut self, item: Packet<B>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        item.write(dst);
        Ok(())
    }
}

impl Decoder for Codec {
    type Error = CodecError;
    // we might want to update the package during codec
    type Item = Packet<BytesMut>;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        Packet::read(src)
    }
}
