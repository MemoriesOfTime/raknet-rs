use bytes::Bytes;
use futures::Stream;

use super::handler::offline;
use crate::codec;
use crate::io::IO;

/// Incoming implementation by using tokio's UDP framework
#[cfg(feature = "tokio-udp")]
mod tokio;

/// Incoming config
#[derive(Debug, Clone, derive_builder::Builder)]
pub struct Config {
    /// The send buffer of each IO polled by the incoming
    send_buf_cap: usize,

    sever_guid: u64,
    advertisement: Bytes,
    min_mtu: u16,
    max_mtu: u16,
    // Supported raknet versions, sorted
    support_version: Vec<u8>,
    max_pending: usize,

    /// Limit the max size of a parted frames set, 0 means no limit
    /// It will abort the split frame if the `parted_size` reaches limit.
    /// Enable it to avoid `DoS` attack.
    /// The maximum number of inflight parted frames is `max_parted_size`*`max_parted_count`nt
    #[builder(default = "256")]
    max_parted_size: u32,
    /// Limit the max count of **all** parted frames sets from an address.
    /// It might cause client resending frames if the limit is reached.
    /// Enable it to avoid `DoS` attack.
    /// The maximum number of inflight parted frames is `max_parted_size`*`max_parted_count`nt
    #[builder(default = "256")]
    max_parted_count: usize,
    /// Maximum ordered channel, the value should be less than 256
    #[builder(default = "1")]
    max_channels: usize,
    // Limit the maximum deduplication gap for a connection, 0 means no limit.
    // Enable it to avoid D-DoS attack based on deduplication.
    #[builder(default = "1024")]
    max_dedup_gap: usize,
}

impl Config {
    fn offline_config(&self) -> offline::Config {
        offline::ConfigBuilder::default()
            .sever_guid(self.sever_guid)
            .advertisement(self.advertisement.clone())
            .min_mtu(self.min_mtu)
            .max_mtu(self.max_mtu)
            .support_version(self.support_version.clone())
            .max_pending(self.max_pending)
            .build()
            .unwrap()
    }

    fn codec_config(&self) -> codec::Config {
        codec::ConfigBuilder::default()
            .max_parted_count(self.max_parted_count)
            .max_parted_size(self.max_parted_size)
            .max_channels(self.max_channels)
            .max_dedup_gap(self.max_dedup_gap)
            .build()
            .unwrap()
    }
}

pub trait MakeIncoming: Sized {
    fn make_incoming(self, config: Config) -> impl Stream<Item = impl IO>;
}
