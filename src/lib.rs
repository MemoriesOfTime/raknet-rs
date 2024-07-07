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
use packet::connected::Reliability;

#[derive(Debug, Clone, Copy)]
enum RoleContext {
    Client { guid: u64 },
    Server { guid: u64 },
}

impl RoleContext {
    fn get_guid(&self) -> u64 {
        match self {
            RoleContext::Client { guid } => *guid,
            RoleContext::Server { guid } => *guid,
        }
    }

    #[cfg(test)]
    fn test_server() -> Self {
        // There is always a server
        RoleContext::Server { guid: 0 }
    }

    #[cfg(test)]
    fn test_client() -> Self {
        // Multiple clients
        RoleContext::Client {
            guid: rand::random(),
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
