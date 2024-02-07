use futures::Stream;

use super::handler::offline;
use crate::{codec, IO};

/// Incoming implementation by using tokio's UDP framework
mod tokio;

/// Incoming config
#[derive(Debug, Clone, derive_builder::Builder)]
pub struct Config {
    /// The send buffer of each IO polled by the incoming
    send_buf_cap: usize,
    codec: codec::Config,
    offline: offline::Config,
}

pub trait MakeIncoming: Sized {
    fn make_incoming(self, config: Config) -> impl Stream<Item = impl IO>;
}
