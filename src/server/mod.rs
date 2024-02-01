use bytes::Bytes;
use futures::{Sink, Stream};

use crate::errors::Error;
use crate::packet::connected::Reliability;

mod handler;
mod incoming;

/// Raknet message
#[derive(Debug, Clone)]
pub struct Message {
    reliability: Reliability,
    order_channel: u8,
    data: Bytes,
}

impl Message {
    pub fn new(reliability: Reliability, order_channel: u8, data: Bytes) -> Self {
        Self {
            reliability,
            order_channel,
            data,
        }
    }

    pub fn set_reliability(&mut self, reliability: Reliability) {
        self.reliability = reliability;
    }

    pub fn set_order_channel(&mut self, channel: u8) {
        self.order_channel = channel;
    }

    pub fn get_reliability(&self) -> Reliability {
        self.reliability
    }

    pub fn get_order_channel(&self) -> u8 {
        self.order_channel
    }

    pub fn get_data(&self) -> &Bytes {
        &self.data
    }

    pub fn into_data(self) -> Bytes {
        self.data
    }
}

// IO Operations
pub trait IOpts {
    fn set_default_reliability(&mut self, reliability: Reliability);
    fn get_default_reliability(&self) -> Reliability;

    fn set_default_order_channel(&mut self, order_channel: u8);
    fn get_default_order_channel(&self) -> u8;
}

// Provide the basic operation for each connection, produced by [`Incoming`]
type IO = impl Stream<Item = Bytes>
    + Sink<Bytes, Error = Error>
    + Sink<Message, Error = Error>
    + IOpts
    + Send
    + Unpin;
