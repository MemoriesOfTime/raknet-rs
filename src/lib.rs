//! Raknet implementation by rust

#![warn(
    clippy::cognitive_complexity,
    clippy::dbg_macro,
    clippy::debug_assert_with_mut_call,
    clippy::doc_link_with_quotes,
    clippy::doc_markdown,
    clippy::empty_line_after_outer_attr,
    clippy::empty_structs_with_brackets,
    clippy::float_cmp,
    clippy::float_cmp_const,
    clippy::float_equality_without_abs,
    keyword_idents,
    missing_copy_implementations,
    missing_debug_implementations,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    non_ascii_idents,
    noop_method_call,
    clippy::option_if_let_else,
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::semicolon_if_nothing_returned,
    clippy::unseparated_literal_suffix,
    clippy::shadow_unrelated,
    clippy::similar_names,
    clippy::suspicious_operation_groupings,
    unused_extern_crates,
    unused_import_braces,
    clippy::unused_self,
    clippy::use_debug,
    clippy::used_underscore_binding,
    clippy::useless_let_if_seq,
    clippy::wildcard_dependencies,
    clippy::wildcard_imports
)]
#![feature(impl_trait_in_assoc_type)]
#![feature(ip_bits)]
#![feature(type_changing_struct_update)]
#![feature(coroutines, proc_macro_hygiene, stmt_expr_attributes)]
#![feature(binary_heap_into_iter_sorted)]
#![feature(let_chains)]
#![feature(noop_waker)]

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

/// Resend map
mod resend_map;

/// Raknet server
pub mod server;

/// Raknet client
pub mod client;

/// The basic operation API
pub mod io;

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

#[derive(Debug, Clone, Copy)]
enum RoleContext {
    Client { guid: u64 },
    Server { guid: u64 },
}

impl RoleContext {
    #[cfg(any(test, feature = "micro-bench"))]
    fn test_server() -> Self {
        // There is always a server
        RoleContext::Server { guid: 0 }
    }

    #[cfg(any(test, feature = "micro-bench"))]
    fn test_client() -> Self {
        // Multiple clients
        RoleContext::Client {
            guid: rand::random(),
        }
    }

    fn guid(&self) -> u64 {
        match self {
            RoleContext::Client { guid } => *guid,
            RoleContext::Server { guid } => *guid,
        }
    }
}

impl std::fmt::Display for RoleContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoleContext::Client { guid } => write!(f, "client({guid})"),
            RoleContext::Server { guid } => write!(f, "server({guid})"),
        }
    }
}

#[derive(Debug, Clone)]
struct PeerContext {
    addr: SocketAddr,
    mtu: u16,
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
