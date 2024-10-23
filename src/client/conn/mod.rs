use std::io;
use std::net::ToSocketAddrs;

use bytes::Bytes;
use futures::{Sink, Stream};

use super::handler::offline;
use crate::opts::{ConnectionInfo, Ping};
use crate::{codec, Message, Role};

/// Connection implementation by using tokio's UDP framework
#[cfg(feature = "tokio-rt")]
mod tokio;

#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Config {
    /// The send buffer of each IO polled by the incoming
    send_buf_cap: usize,
    /// The given mtu, the default value is 1400
    mtu: u16,
    /// The client guid, used to identify the client, initialized by random
    client_guid: u64,
    /// Raknet protocol version, default is 9
    protocol_version: u8,
    /// Limit the max size of a parted frames set, 0 means no limit
    /// It will abort the split frame if the `parted_size` reaches limit.
    /// The maximum number of inflight parted frames is `max_parted_size` * `max_parted_count`
    max_parted_size: u32,
    /// Limit the max count of **all** parted frames sets from an address.
    /// It might cause client resending frames if the limit is reached.
    /// The maximum number of inflight parted frames is `max_parted_size` * `max_parted_count`
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
            mtu: 1400,
            client_guid: rand::random(),
            protocol_version: 9,
            max_parted_size: 256,
            max_parted_count: 256,
            max_channels: 1,
        }
    }

    /// Set the send buffer capacity of each IO polled by the incoming
    pub fn send_buf_cap(mut self, send_buf_cap: usize) -> Self {
        self.send_buf_cap = send_buf_cap;
        self
    }

    /// Give the mtu of the connection
    pub fn mtu(mut self, mtu: u16) -> Self {
        self.mtu = mtu;
        self
    }

    /// Set the client guid
    pub fn client_guid(mut self, client_guid: u64) -> Self {
        self.client_guid = client_guid;
        self
    }

    /// Set the protocol version
    pub fn protocol_version(mut self, protocol_version: u8) -> Self {
        self.protocol_version = protocol_version;
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
            client_guid: self.client_guid,
            mtu: self.mtu,
            protocol_version: self.protocol_version,
        }
    }

    fn codec_config(&self) -> codec::Config {
        codec::Config {
            max_parted_count: self.max_parted_count,
            max_parted_size: self.max_parted_size,
            max_channels: self.max_channels,
        }
    }

    fn client_role(&self) -> Role {
        Role::Client {
            guid: self.client_guid,
        }
    }
}

pub trait ConnectTo: Sized {
    #[allow(async_fn_in_trait)] // No need to consider the auto trait for now.
    async fn connect_to(
        self,
        addr: impl ToSocketAddrs,
        config: Config,
    ) -> io::Result<(
        impl Stream<Item = Bytes>,
        impl Sink<Message, Error = io::Error> + Ping + ConnectionInfo,
    )>;
}
