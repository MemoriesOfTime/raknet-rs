use std::hash::{Hash, Hasher};

use bytes::{Buf, BufMut, Bytes, BytesMut};

use super::Uint24le;
use crate::errors::CodecError;
use crate::packet::{PackType, SocketAddrRead, SocketAddrWrite, NEEDS_B_AND_AS_FLAG, PARTED_FLAG};
use crate::read_buf;

#[derive(Eq, PartialEq, Clone)]
pub(crate) struct Frame<B> {
    pub(crate) flags: Flags,
    pub(crate) reliable_frame_index: Option<Uint24le>,
    pub(crate) seq_frame_index: Option<Uint24le>,
    pub(crate) ordered: Option<Ordered>,
    pub(crate) fragment: Option<Fragment>,
    pub(crate) body: B,
}

impl<B> std::fmt::Debug for Frame<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("flags", &self.flags)
            .field("reliable_frame_index", &self.reliable_frame_index)
            .field("seq_frame_index", &self.seq_frame_index)
            .field("ordered", &self.ordered)
            .field("fragment", &self.fragment)
            .finish()
    }
}

impl<B: Hash> Hash for Frame<B> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // if it is a parted frame, then hash the fragment parted_index
        // to promise that the same parted_index will be hashed to the same frame
        // in `frames_queue`
        if let Some(fragment) = &self.fragment {
            fragment.parted_index.hash(state);
            return;
        }
        self.flags.hash(state);
        self.reliable_frame_index.hash(state);
        self.seq_frame_index.hash(state);
        self.ordered.hash(state);
        self.body.hash(state);
    }
}

impl Frame<BytesMut> {
    pub(crate) fn freeze(self) -> Frame<Bytes> {
        Frame {
            body: self.body.freeze(),
            ..self
        }
    }

    /// Remove the parted flags & fragment payload
    pub(crate) fn reassembled(&mut self) {
        if !self.flags.parted {
            return;
        }
        self.flags.raw &= PARTED_FLAG.reverse_bits();
        self.flags.parted = false;
        self.fragment = None;
    }

    fn read(buf: &mut BytesMut) -> Result<Self, CodecError> {
        let (flags, length) = read_buf!(buf, 3, {
            let flags = Flags::read(buf);
            // length in bytes
            let length = buf.get_u16() >> 3;
            if length == 0 {
                return Err(CodecError::InvalidPacketLength("frame length"));
            }
            (flags, length as usize)
        });
        let reliability = flags.reliability;
        let mut reliable_frame_index = None;
        let mut seq_frame_index = None;
        let mut ordered = None;

        let mut fragment = None;

        if reliability.is_reliable() {
            reliable_frame_index = read_buf!(buf, 3, Some(Uint24le::read(buf)));
        }
        if reliability.is_sequenced() {
            seq_frame_index = read_buf!(buf, 3, Some(Uint24le::read(buf)));
        }
        if reliability.is_sequenced_or_ordered() {
            ordered = read_buf!(buf, 4, Some(Ordered::read(buf)));
        }
        if flags.parted() {
            fragment = read_buf!(buf, 10, Some(Fragment::read(buf)));
        }
        let body = read_buf!(buf, length, buf.split_to(length));
        Ok(Frame {
            flags,
            reliable_frame_index,
            seq_frame_index,
            ordered,
            fragment,
            body,
        })
    }
}

impl<B: Buf> Frame<B> {
    fn write(self, buf: &mut BytesMut) {
        self.flags.write(buf);
        // length in bits
        // self.body will be split up so cast to u16 should not overflow here
        debug_assert!(
            self.body.remaining() < (u16::MAX >> 3) as usize,
            "self.body should be constructed based on mtu"
        );
        buf.put_u16((self.body.remaining() << 3) as u16);
        if let Some(reliable_frame_index) = self.reliable_frame_index {
            reliable_frame_index.write(buf);
        }
        if let Some(seq_frame_index) = self.seq_frame_index {
            seq_frame_index.write(buf);
        }
        if let Some(ordered) = self.ordered {
            ordered.write(buf);
        }
        if let Some(fragment) = self.fragment {
            fragment.write(buf);
        }
        buf.put(self.body);
    }
}

#[derive(Debug, PartialEq, Clone)]
pub(crate) struct FrameSet<B> {
    pub(crate) seq_num: Uint24le,
    pub(crate) frames: Vec<Frame<B>>,
}

impl FrameSet<BytesMut> {
    pub(super) fn read(buf: &mut BytesMut) -> Result<Self, CodecError> {
        let seq_num = read_buf!(buf, 3, Uint24le::read(buf));
        let mut frames = Vec::new();
        while buf.has_remaining() {
            frames.push(Frame::read(buf)?);
        }
        if frames.is_empty() {
            return Err(CodecError::InvalidPacketLength("frame set"));
        }
        Ok(FrameSet { seq_num, frames })
    }

    pub(super) fn freeze(self) -> FrameSet<Bytes> {
        FrameSet {
            seq_num: self.seq_num,
            frames: self.frames.into_iter().map(Frame::freeze).collect(),
        }
    }
}

impl<B: Buf> FrameSet<B> {
    pub(super) fn write(self, buf: &mut BytesMut) {
        self.seq_num.write(buf);
        for frame in self.frames {
            frame.write(buf);
        }
    }
}

/// Top 3 bits are reliability type, fourth bit is 1 when the frame is fragmented and part of a
/// compound.
#[derive(Debug, Eq, PartialEq, Clone)]
pub(crate) struct Flags {
    raw: u8,
    reliability: Reliability,
    parted: bool,
    needs_bas: bool,
}

impl Hash for Flags {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Copy)]
#[repr(u8)]
pub(crate) enum Reliability {
    /// Direct UDP
    Unreliable = 0b000,

    /// Ordered
    UnreliableSequenced = 0b001,

    /// Deduplicated
    Reliable = 0b010,

    /// Ordered + Deduplicated + Resend  (Most used)
    ReliableOrdered = 0b011,

    /// Ordered + Deduplicated
    ReliableSequenced = 0b100,

    /// Not used
    UnreliableWithAckReceipt = 0b101,
    UnreliableSequencedWithAckReceipt = 0b110,
    ReliableWithAckReceipt = 0b111,

    /// Defined but never be used (cannot be used).
    ReliableOrderedWithAckReceipt = 0b1_000,
    ReliableSequencedWithAckReceipt = 0b1_001,
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
                | Reliability::ReliableSequencedWithAckReceipt
        )
    }

    /// Sequenced or Ordered ensures that packets should be received in order as they are sent.
    pub(crate) fn is_sequenced_or_ordered(&self) -> bool {
        matches!(
            self,
            Reliability::ReliableSequenced
                | Reliability::ReliableOrdered
                | Reliability::UnreliableSequenced
                | Reliability::UnreliableSequencedWithAckReceipt
                | Reliability::ReliableSequencedWithAckReceipt
                | Reliability::ReliableOrderedWithAckReceipt
        )
    }

    /// No effect?
    pub(crate) fn is_sequenced(&self) -> bool {
        matches!(
            self,
            Reliability::UnreliableSequenced
                | Reliability::ReliableSequenced
                | Reliability::UnreliableSequencedWithAckReceipt
                | Reliability::ReliableSequencedWithAckReceipt
        )
    }
}

impl Flags {
    fn read(buf: &mut BytesMut) -> Self {
        let raw = buf.get_u8();
        Self::parse(raw)
    }

    fn write(self, buf: &mut BytesMut) {
        buf.put_u8(self.raw);
    }

    pub(crate) fn parse(raw: u8) -> Self {
        let r = raw >> 5;
        // Safety:
        // It is checked before transmute
        Self {
            raw,
            reliability: unsafe { std::mem::transmute(r) },
            parted: raw & PARTED_FLAG != 0,
            needs_bas: raw & NEEDS_B_AND_AS_FLAG != 0,
        }
    }

    /// Get the reliability of this flags
    pub(crate) fn reliability(&self) -> Reliability {
        self.reliability
    }

    /// Return if it is parted
    pub(crate) fn parted(&self) -> bool {
        self.parted
    }

    pub(crate) fn needs_bas(&self) -> bool {
        self.needs_bas
    }
}

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub(crate) struct Fragment {
    pub(crate) parted_size: u32,
    pub(crate) parted_id: u16,
    pub(crate) parted_index: u32,
}

impl Fragment {
    fn read(buf: &mut BytesMut) -> Self {
        Self {
            parted_size: buf.get_u32(),
            parted_id: buf.get_u16(),
            parted_index: buf.get_u32(),
        }
    }

    fn write(self, buf: &mut BytesMut) {
        buf.put_u32(self.parted_size);
        buf.put_u16(self.parted_id);
        buf.put_u32(self.parted_index);
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub(crate) struct Ordered {
    pub(crate) frame_index: Uint24le,
    pub(crate) channel: u8,
}

impl Ordered {
    fn read(buf: &mut BytesMut) -> Self {
        Self {
            frame_index: Uint24le::read(buf),
            channel: buf.get_u8(),
        }
    }

    fn write(self, buf: &mut BytesMut) {
        self.frame_index.write(buf);
        buf.put_u8(self.channel);
    }
}

#[derive(Clone)]
pub(crate) enum FrameBody {
    ConnectedPing {
        client_timestamp: i64,
    },
    ConnectedPong {
        client_timestamp: i64,
        server_timestamp: i64,
    },
    ConnectionRequest {
        client_guid: u64,
        request_timestamp: i64,
        use_encryption: bool,
    },
    ConnectionRequestAccepted {
        client_address: std::net::SocketAddr,
        system_index: u16,
        system_addresses: [std::net::SocketAddr; 10],
        request_timestamp: i64,
        accepted_timestamp: i64,
    },
    NewIncomingConnection {
        server_address: std::net::SocketAddr,
        system_addresses: [std::net::SocketAddr; 10],
        request_timestamp: i64,
        accepted_timestamp: i64,
    },
    Disconnect,
    Game(Bytes),
}

impl std::fmt::Debug for FrameBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectedPing { client_timestamp } => f
                .debug_struct("ConnectedPing")
                .field("client_timestamp", client_timestamp)
                .finish(),
            Self::ConnectedPong {
                client_timestamp,
                server_timestamp,
            } => f
                .debug_struct("ConnectedPong")
                .field("client_timestamp", client_timestamp)
                .field("server_timestamp", server_timestamp)
                .finish(),
            Self::ConnectionRequest {
                client_guid,
                request_timestamp,
                use_encryption,
            } => f
                .debug_struct("ConnectionRequest")
                .field("client_guid", client_guid)
                .field("request_timestamp", request_timestamp)
                .field("use_encryption", use_encryption)
                .finish(),
            Self::ConnectionRequestAccepted {
                client_address,
                system_index,
                system_addresses,
                request_timestamp,
                accepted_timestamp,
            } => f
                .debug_struct("ConnectionRequestAccepted")
                .field("client_address", client_address)
                .field("system_index", system_index)
                .field("system_addresses", system_addresses)
                .field("request_timestamp", request_timestamp)
                .field("accepted_timestamp", accepted_timestamp)
                .finish(),
            Self::NewIncomingConnection {
                server_address,
                system_addresses,
                request_timestamp,
                accepted_timestamp,
            } => f
                .debug_struct("NewIncomingConnection")
                .field("server_address", server_address)
                .field("system_addresses", system_addresses)
                .field("request_timestamp", request_timestamp)
                .field("accepted_timestamp", accepted_timestamp)
                .finish(),
            Self::Disconnect => write!(f, "Disconnect"),
            Self::Game(data) => write!(f, "Game(data_size:{})", data.remaining()),
        }
    }
}

impl FrameBody {
    pub(crate) fn read(mut buf: Bytes) -> Result<Self, CodecError> {
        let id = PackType::from_u8(buf.get_u8())?;
        match id {
            PackType::ConnectedPing => Ok(Self::ConnectedPing {
                client_timestamp: buf.get_i64(),
            }),
            PackType::ConnectedPong => Ok(Self::ConnectedPong {
                client_timestamp: buf.get_i64(),
                server_timestamp: buf.get_i64(),
            }),
            PackType::ConnectionRequest => Ok(Self::ConnectionRequest {
                client_guid: buf.get_u64(),
                request_timestamp: buf.get_i64(),
                use_encryption: buf.get_u8() != 0,
            }),
            PackType::ConnectionRequestAccepted => Ok(Self::ConnectionRequestAccepted {
                client_address: buf.get_socket_addr()?,
                system_index: buf.get_u16(),
                system_addresses: todo!(),
                request_timestamp: buf.get_i64(),
                accepted_timestamp: buf.get_i64(),
            }),
            PackType::NewIncomingConnection => Ok(Self::NewIncomingConnection {
                server_address: buf.get_socket_addr()?,
                system_addresses: todo!(),
                request_timestamp: buf.get_i64(),
                accepted_timestamp: buf.get_i64(),
            }),
            PackType::DisconnectNotification => Ok(Self::Disconnect),
            PackType::Game => Ok(Self::Game(buf)),
            _ => Err(CodecError::InvalidPacketType(id.into())),
        }
    }

    pub(crate) fn write(self, buf: &mut BytesMut) {
        match self {
            FrameBody::ConnectedPing { client_timestamp } => {
                buf.put_i64(client_timestamp);
            }
            FrameBody::ConnectedPong {
                client_timestamp,
                server_timestamp,
            } => {
                buf.put_i64(client_timestamp);
                buf.put_i64(server_timestamp);
            }
            FrameBody::ConnectionRequest {
                client_guid,
                request_timestamp,
                use_encryption,
            } => {
                buf.put_u64(client_guid);
                buf.put_i64(request_timestamp);
                buf.put_u8(u8::from(use_encryption));
            }
            FrameBody::ConnectionRequestAccepted {
                client_address,
                system_index,
                system_addresses,
                request_timestamp,
                accepted_timestamp,
            } => {
                buf.put_socket_addr(client_address);
                buf.put_u16(system_index);
                for addr in system_addresses {
                    buf.put_socket_addr(addr);
                }
                buf.put_i64(request_timestamp);
                buf.put_i64(accepted_timestamp);
            }
            FrameBody::NewIncomingConnection {
                server_address,
                system_addresses,
                request_timestamp,
                accepted_timestamp,
            } => {
                buf.put_socket_addr(server_address);
                for addr in system_addresses {
                    buf.put_socket_addr(addr);
                }
                buf.put_i64(request_timestamp);
                buf.put_i64(accepted_timestamp);
            }
            FrameBody::Disconnect => {}
            FrameBody::Game(data) => {
                buf.put(data);
            }
        }
    }
}
