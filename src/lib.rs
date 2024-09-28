//! Raknet implementation by rust

#![feature(impl_trait_in_assoc_type)]
#![feature(ip_bits)]
#![feature(type_changing_struct_update)]
#![feature(coroutines, proc_macro_hygiene, stmt_expr_attributes)]
#![feature(binary_heap_into_iter_sorted)]
#![feature(let_chains)]
#![feature(context_ext)]
#![feature(local_waker)]
#![feature(option_get_or_insert_default)]

/// Protocol codec
mod codec;

/// Errors
mod errors;

/// Protocol packet
mod packet;

/// Utils
mod utils;

/// Outgoing guard
mod guard;

/// Sink & Stream state
mod state;

/// Transfer link
mod link;

/// Estimators
mod estimator;

/// Raknet server
pub mod server;

/// Raknet client
pub mod client;

/// Connection optional settings
pub mod opts;

#[cfg(feature = "micro-bench")]
pub mod micro_bench {
    pub mod codec {
        pub use crate::codec::micro_bench::*;
    }
}

/// Unit tests
#[cfg(test)]
mod tests;

use std::net::SocketAddr;

use bytes::Bytes;

/// The `Role` enum is used to identify the `Client` and `Server`, and it stores their GUID.
/// The GUID is a globally unique identifier that is not affected by changes to IP address or port.
/// It is application-defined and ensures unique identification.
#[derive(Debug, Clone, Copy)]
enum Role {
    Client { guid: u64 },
    Server { guid: u64 },
}

impl Role {
    #[cfg(test)]
    fn test_server() -> Self {
        Role::Server { guid: 114514 }
    }

    fn guid(&self) -> u64 {
        match self {
            Role::Client { guid } => *guid,
            Role::Server { guid } => *guid,
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Role::Client { guid } => write!(f, "client({guid})"),
            Role::Server { guid } => write!(f, "server({guid})"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Peer {
    guid: u64,
    addr: SocketAddr,
    mtu: u16,
}

impl Peer {
    #[cfg(test)]
    fn test() -> Self {
        Self {
            guid: 114514,
            addr: SocketAddr::from(([11, 45, 14, 19], 19810)),
            mtu: 1919,
        }
    }
}

impl std::fmt::Display for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{guid}@{addr}", guid = self.guid, addr = self.addr,)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Copy)]
#[repr(u8)]
pub enum Reliability {
    /// Same as regular UDP, except that it will also discard duplicate datagrams. `RakNet` adds
    /// (6 to 17) + 21 bits of overhead, 16 of which is used to detect duplicate packets and 6
    /// to 17 of which is used for message length.
    Unreliable = 0b000,

    /// Regular UDP with a sequence counter.  Out of order messages will be discarded.
    /// Sequenced and ordered messages sent on the same channel will arrive in the order sent.
    UnreliableSequenced = 0b001,

    /// The message is sent reliably, but not necessarily in any order.  Same overhead as
    /// UNRELIABLE.
    Reliable = 0b010,

    /// This message is reliable and will arrive in the order you sent it.  Messages will be
    /// delayed while waiting for out of order messages.  Same overhead as `UnreliableSequenced`.
    /// Sequenced and ordered messages sent on the same channel will arrive in the order sent.
    ReliableOrdered = 0b011,

    /// This message is reliable and will arrive in the sequence you sent it.  Out of order
    /// messages will be dropped.  Same overhead as `UnreliableSequenced`. Sequenced and ordered
    /// messages sent on the same channel will arrive in the order sent.
    ReliableSequenced = 0b100,

    /// Same as Unreliable, however the peer will get either ACK or
    /// NACK based on the result of sending this message when calling.
    UnreliableWithAckReceipt = 0b101,

    /// Same as Reliable, however the peer will get either ACK or
    /// NACK based on the result of sending this message when calling.
    ReliableWithAckReceipt = 0b110,

    /// Same as `ReliableOrdered`, however the peer will get either ACK or
    /// NACK based on the result of sending this message when calling.
    ReliableOrderedWithAckReceipt = 0b111,
}

impl Reliability {
    /// Reliable ensures that the packet is not duplicated.
    pub(crate) fn is_reliable(&self) -> bool {
        matches!(
            self,
            Reliability::Reliable
                | Reliability::ReliableSequenced
                | Reliability::ReliableOrdered
                | Reliability::ReliableWithAckReceipt
                | Reliability::ReliableOrderedWithAckReceipt
        )
    }

    /// Sequenced or Ordered ensures that packets should be received in order at their
    /// `order_channels` as they are sent.
    pub(crate) fn is_sequenced_or_ordered(&self) -> bool {
        matches!(
            self,
            Reliability::ReliableSequenced
                | Reliability::ReliableOrdered
                | Reliability::UnreliableSequenced
                | Reliability::ReliableOrderedWithAckReceipt
        )
    }

    /// TODO: implement sequenced
    pub(crate) fn is_sequenced(&self) -> bool {
        matches!(
            self,
            Reliability::UnreliableSequenced | Reliability::ReliableSequenced
        )
    }

    /// The header size (without fragment part) implied from reliability
    pub(crate) fn size(&self) -> usize {
        // flag(1B) + length(2B)
        let mut size = 3;
        if self.is_reliable() {
            size += 3;
        }
        if self.is_sequenced() {
            size += 3;
        }
        if self.is_sequenced_or_ordered() {
            size += 4;
        }
        size
    }
}

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
