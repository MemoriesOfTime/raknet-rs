mod decoder;

use bytes::{Buf, BytesMut};
use derive_builder::Builder;
use futures::{Stream, StreamExt};
use tokio_util::codec::{Decoder, Encoder};
use tracing::{debug, trace};

use self::decoder::{DeFragmented, Deduplicated, FrameDecoded, Ordered};
use crate::codec::decoder::AckDispatched;
use crate::errors::CodecError;
use crate::packet::connected::{FrameBody, Frames};
use crate::packet::{connected, Packet};
use crate::utils::Logged;

/// Codec config
#[derive(Clone, Copy, Debug, Builder)]
pub struct Config {
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

impl Default for Config {
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
    fn decoded(self, config: Config) -> impl Stream<Item = FrameBody>;
}

impl<F> Decoded for F
where
    F: Stream<Item = connected::Packet<Frames<BytesMut>>>,
{
    fn decoded(self, config: Config) -> impl Stream<Item = FrameBody> {
        fn ok_f(pack: &FrameBody) {
            trace!("[decoder] received packet: {:?}", pack);
        }
        fn err_f(err: CodecError) {
            debug!("[decoder] got codec error: {err} when decode packet");
        }

        let (ack_tx, ack_rx) = flume::unbounded();
        let (nack_tx, nack_rx) = flume::unbounded();

        self.map(Ok)
            .dispatch_recv_ack(ack_tx, nack_tx)
            .deduplicated(config.max_dedup_gap)
            .defragmented(config.max_parted_size, config.max_parted_count)
            .ordered(config.max_channels)
            .frame_decoded()
            .logged_all(ok_f, err_f)
    }
}

/// The raknet codec
pub(crate) struct Codec;

impl<B: Buf> Encoder<Packet<Frames<B>>> for Codec {
    type Error = CodecError;

    fn encode(&mut self, item: Packet<Frames<B>>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        item.write(dst);
        Ok(())
    }
}

impl Decoder for Codec {
    type Error = CodecError;
    // we might want to update the package during codec
    type Item = Packet<Frames<BytesMut>>;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        Packet::read(src)
    }
}
