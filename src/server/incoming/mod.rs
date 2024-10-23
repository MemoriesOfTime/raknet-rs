use std::io;

use bytes::Bytes;
use futures::{Sink, Stream};

use super::handler::offline;
use crate::opts::{ConnectionInfo, TraceInfo};
use crate::{codec, Message, Role};

/// Incoming implementation by using tokio's UDP framework
#[cfg(feature = "tokio-rt")]
mod tokio;

/// Incoming config
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Config {
    /// The send buffer of each IO polled by the incoming
    send_buf_cap: usize,
    /// The server guid, used to identify the server, initialized by random
    sever_guid: u64,
    /// The advertisement, sent to the client when the client pings the server
    advertisement: String,
    /// The minimum mtu, the default value is 510
    min_mtu: u16,
    /// The maximum mtu, the default value is 1500
    max_mtu: u16,
    /// Supported raknet versions, sorted
    support_version: Vec<u8>,
    /// The maximum pending(aka. half-opened connections)
    max_pending: usize,
    /// Limit the max size of a parted frames set, 0 means no limit
    /// It will abort the split frame if the `parted_size` reaches limit.
    /// Enable it to avoid `DoS` attack.
    /// The maximum number of inflight parted frames is `max_parted_size`*`max_parted_count`nt
    max_parted_size: u32,
    /// Limit the max count of **all** parted frames sets from an address.
    /// It might cause client resending frames if the limit is reached.
    /// Enable it to avoid `DoS` attack.
    /// The maximum number of inflight parted frames is `max_parted_size`*`max_parted_count`nt
    max_parted_count: usize,
    /// Maximum ordered channel, the value should be less than 256
    max_channels: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

impl Config {
    pub fn new() -> Self {
        Self {
            send_buf_cap: 1024,
            sever_guid: rand::random(),
            advertisement: String::new(),
            min_mtu: 510,
            max_mtu: 1500,
            support_version: vec![9, 11, 13],
            max_pending: 1024,
            max_parted_size: 256,
            max_parted_count: 256,
            max_channels: 1,
        }
    }

    /// Set the send buffer of each IO polled by the incoming
    /// The default value is 1024
    pub fn send_buf_cap(mut self, cap: usize) -> Self {
        self.send_buf_cap = cap;
        self
    }

    /// Set the server guid
    /// The default value is random
    pub fn sever_guid(mut self, guid: u64) -> Self {
        self.sever_guid = guid;
        self
    }

    /// Set the advertisement
    /// The default value is empty
    pub fn advertisement(mut self, advertisement: impl ToString) -> Self {
        self.advertisement = advertisement.to_string();
        self
    }

    /// Set the minimum mtu
    /// The default value is 510
    pub fn min_mtu(mut self, mtu: u16) -> Self {
        self.min_mtu = mtu;
        self
    }

    /// Set the maximum mtu
    /// The default value is 1500
    pub fn max_mtu(mut self, mtu: u16) -> Self {
        self.max_mtu = mtu;
        self
    }

    /// Set the supported raknet versions
    /// The default value is [9, 11, 13]
    pub fn support_version(mut self, mut version: Vec<u8>) -> Self {
        version.sort();
        self.support_version = version;
        self
    }

    /// Set the maximum pending(aka. half-opened connections)
    /// The default value is 1024
    pub fn max_pending(mut self, pending: usize) -> Self {
        self.max_pending = pending;
        self
    }

    /// Set the maximum parted size
    /// The default value is 256
    /// The maximum number of inflight parted frames is `max_parted_size`*`max_parted_count`nt
    pub fn max_parted_size(mut self, size: u32) -> Self {
        self.max_parted_size = size;
        self
    }

    /// Set the maximum parted count
    /// The default value is 256
    /// The maximum number of inflight parted frames is `max_parted_size`*`max_parted_count`nt
    pub fn max_parted_count(mut self, count: usize) -> Self {
        self.max_parted_count = count;
        self
    }

    /// Set the maximum channels
    /// The default value is 1
    /// The maximum value should be less than 256
    /// # Panics
    /// Panics if the channels is greater than 256
    pub fn max_channels(mut self, channels: usize) -> Self {
        assert!(channels < 256, "max_channels should be less than 256");
        self.max_channels = channels;
        self
    }

    fn offline_config(&self) -> offline::Config {
        offline::Config {
            sever_guid: self.sever_guid,
            advertisement: Bytes::from_iter(self.advertisement.bytes()),
            min_mtu: self.min_mtu,
            max_mtu: self.max_mtu,
            support_version: self.support_version.clone(),
            max_pending: self.max_pending,
        }
    }

    fn codec_config(&self) -> codec::Config {
        codec::Config {
            max_parted_count: self.max_parted_count,
            max_parted_size: self.max_parted_size,
            max_channels: self.max_channels,
        }
    }

    fn server_role(&self) -> Role {
        Role::Server {
            guid: self.sever_guid,
        }
    }
}

pub trait MakeIncoming: Sized {
    fn make_incoming(
        self,
        config: Config,
    ) -> impl Stream<
        Item = (
            impl Stream<Item = Bytes> + TraceInfo,
            impl Sink<Message, Error = io::Error> + ConnectionInfo,
        ),
    >;
}
