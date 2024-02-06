use std::net::SocketAddr;

use super::handler::offline;
use crate::codec;

/// Connection implementation by using tokio's UDP framework
mod tokio;

pub type IO = impl crate::IO;

#[derive(Debug, Clone, derive_builder::Builder)]
pub struct Config {
    send_buf_cap: usize,
    codec: codec::Config,
    offline: offline::Config,
}

pub trait ConnectTo: Sized {
    fn connect_to(self, addr: SocketAddr, config: Config) -> IO;
}
