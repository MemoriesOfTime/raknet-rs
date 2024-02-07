use std::net::ToSocketAddrs;

use super::handler::offline;
use crate::errors::Error;
use crate::{codec, IO};

/// Connection implementation by using tokio's UDP framework
mod tokio;

#[derive(Debug, Clone, derive_builder::Builder)]
pub struct Config {
    send_buf_cap: usize,
    codec: codec::Config,
    offline: offline::Config,
}

pub trait ConnectTo: Sized {
    async fn connect_to(self, addr: impl ToSocketAddrs, config: Config) -> Result<impl IO, Error>;
}
