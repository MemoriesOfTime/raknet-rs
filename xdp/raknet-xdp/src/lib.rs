use aya::include_bytes_aligned;

pub const DEFAULT_PROGRAM: &[u8] =
    include_bytes_aligned!(concat!(env!("OUT_DIR"), "/raknet-xdp-ebpf"));
pub const DEFAULT_PROGRAM_NAME: &str = "raknet_xdp";

/// The name of the `AF_XDP` socket map
pub static XSK_MAP_NAME: &str = "RAKNET_XDP_SOCKETS";

/// The name of the port map
pub static PORT_MAP_NAME: &str = "RAKNET_XDP_PORTS";

type Result<T = (), E = std::io::Error> = core::result::Result<T, E>;

#[rustfmt::skip]
mod bindings;

mod if_xdp;
mod io;
mod mmap;
mod socket;
mod syscall;
mod umem;
