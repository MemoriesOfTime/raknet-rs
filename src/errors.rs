#[derive(thiserror::Error, Debug)]
pub enum CodecError {
    #[error("io error {0}")]
    IO(#[from] std::io::Error),
    #[error("invalid ip version {0}")]
    InvalidIPVer(u8),
    #[error("expect IPv6 family 0x17, got {0}")]
    InvalidIPV6Family(u16),
    #[error("invalid packet length when decode {0}")]
    InvalidPacketLength(&'static str),
    #[error("invalid record type {0}")]
    InvalidRecordType(u8),
    #[error("invalid packet type {0}, maybe it is a user packet")]
    InvalidPacketType(u8),
    #[error("parted frame error, reason: {0}")]
    PartedFrame(String),
    #[error("ordered frame error, reason: {0}")]
    OrderedFrame(String),
    #[error("maximum amount of packets in acknowledgement exceeded")]
    AckCountExceed,
    #[error("exceed deduplication maximum gap {0}, current gap {1}")]
    DedupExceed(usize, usize),
    #[error("magic number not matched, pos {0}, byte {1}")]
    MagicNotMatched(usize, u8),
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("io error {0}")]
    IO(#[from] std::io::Error),
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error("connection closed")]
    ConnectionClosed,
}
