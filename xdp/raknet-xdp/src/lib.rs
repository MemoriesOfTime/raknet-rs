use aya::include_bytes_aligned;

pub const DEFAULT_PROGRAM: &[u8] =
    include_bytes_aligned!(concat!(env!("OUT_DIR"), "/raknet-xdp-ebpf"));
pub const DEFAULT_PROGRAM_NAME: &str = "raknet_xdp";
