use std::net::ToSocketAddrs;

use super::handler::offline;
use crate::errors::Error;
use crate::{codec, IO};

/// Connection implementation by using tokio's UDP framework
#[cfg(feature = "tokio-udp")]
mod tokio;

#[derive(Debug, Clone, Copy, derive_builder::Builder)]
pub struct Config {
    /// The send buffer of each IO polled by the incoming
    send_buf_cap: usize,

    mtu: u16,
    client_guid: u64,
    protocol_version: u8,

    /// Limit the max size of a parted frames set, 0 means no limit
    /// It will abort the split frame if the parted_size reaches limit.
    /// Enable it to avoid DoS attack.
    /// The maximum number of inflight parted frames is max_parted_size * max_parted_count
    #[builder(default = "256")]
    max_parted_size: u32,
    /// Limit the max count of **all** parted frames sets from an address.
    /// It might cause client resending frames if the limit is reached.
    /// Enable it to avoid DoS attack.
    /// The maximum number of inflight parted frames is max_parted_size * max_parted_count
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
            .mtu(self.mtu)
            .client_guid(self.client_guid)
            .protocol_version(self.protocol_version)
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

pub trait ConnectTo: Sized {
    #[allow(async_fn_in_trait)] // No need to consider the auto trait for now.
    async fn connect_to(self, addr: impl ToSocketAddrs, config: Config) -> Result<impl IO, Error>;
}
