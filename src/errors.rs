/// Result type in raknet
pub type Result<T> = std::result::Result<T, Error>;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("codec error {0}")]
    Codec(#[from] CodecError),
    #[error("io error {0}")]
    IO(#[from] std::io::Error),
}

#[derive(thiserror::Error, Debug)]
pub enum CodecError {
    #[error("io error {0}")]
    IO(#[from] std::io::Error),
    #[error("invalid ip version {0}")]
    InvalidIPVer(u8),
    #[error("expect IPv6 family 0x17, got {0}")]
    InvalidIPV6Family(u16),
    #[error("invalid reliability flags {0}")]
    InvalidReliability(u8),
    #[error("invalid packet length")]
    InvalidPacketLength,
    #[error("maximum amount of packets in acknowledgement exceeded")]
    AckCountExceed,
    #[error("invalid record type {0}")]
    InvalidRecordType(u8),
    #[error("invalid packet id {0}")]
    InvalidPacketId(u8),
    #[error("parted frame error, reason: {0}")]
    PartedFrame(String),
}
