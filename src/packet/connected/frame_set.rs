use std::net::SocketAddr;

use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::errors::CodecError;
use crate::packet::{
    read_buf, PackType, SocketAddrRead, SocketAddrWrite, FRAGMENT_PART_SIZE, NEEDS_B_AND_AS_FLAG,
    PARTED_FLAG,
};
use crate::utils::{u24, BufExt, BufMutExt};

pub(crate) type Frames<B = Bytes> = Vec<Frame<B>>;

// Cheap slice of a frames vector to reduce heap allocation
pub(crate) type FramesRef<'a, B = Bytes> = &'a [Frame<B>];

pub(crate) type FramesMut = Frames<BytesMut>;

pub(crate) type FrameMut = Frame<BytesMut>;

#[derive(Debug, PartialEq, Clone)]
pub(crate) struct FrameSet<S> {
    pub(crate) seq_num: u24,
    pub(crate) set: S,
}

impl FrameSet<FramesMut> {
    pub(super) fn read(buf: &mut BytesMut) -> Result<Self, CodecError> {
        let seq_num = read_buf!(buf, 3, buf.get_u24_le());
        let mut frames = vec![];
        while buf.has_remaining() {
            frames.push(Frame::read(buf)?);
        }
        if frames.is_empty() {
            return Err(CodecError::InvalidPacketLength("frame set"));
        }
        Ok(FrameSet {
            seq_num,
            set: frames,
        })
    }
}

impl<B: Buf> FrameSet<Frames<B>> {
    pub(super) fn write(self, buf: &mut BytesMut) {
        buf.put_u24_le(self.seq_num);
        for frame in self.set {
            frame.write(buf);
        }
    }
}

impl<'a, B: Buf + Clone> FrameSet<FramesRef<'a, B>> {
    pub(super) fn write(self, buf: &mut BytesMut) {
        buf.put_u24_le(self.seq_num);
        for frame in self.set {
            frame.write_ref(buf);
        }
    }
}

#[derive(PartialEq, Clone)]
pub(crate) struct Frame<B = Bytes> {
    pub(crate) flags: Flags,
    pub(crate) reliable_frame_index: Option<u24>,
    pub(crate) seq_frame_index: Option<u24>,
    pub(crate) ordered: Option<Ordered>,
    pub(crate) fragment: Option<Fragment>,
    pub(crate) body: B,
}

impl<B: Buf> std::fmt::Debug for Frame<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("flags", &self.flags)
            .field("reliable_frame_index", &self.reliable_frame_index)
            .field("seq_frame_index", &self.seq_frame_index)
            .field("ordered", &self.ordered)
            .field("fragment", &self.fragment)
            .field("body", &self.body.chunk())
            .finish()
    }
}

impl FrameMut {
    pub(crate) fn freeze(self) -> Frame {
        Frame {
            body: self.body.freeze(),
            ..self
        }
    }

    /// Remove the parted flags & fragment payload
    pub(crate) fn reassembled(mut self) -> Self {
        if !self.flags.parted {
            return self;
        }
        self.flags.raw &= PARTED_FLAG.reverse_bits();
        self.flags.parted = false;
        self.fragment = None;
        self
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
            reliable_frame_index = read_buf!(buf, 3, Some(buf.get_u24_le()));
        }
        if reliability.is_sequenced() {
            seq_frame_index = read_buf!(buf, 3, Some(buf.get_u24_le()));
        }
        if reliability.is_sequenced_or_ordered() {
            ordered = read_buf!(buf, 4, Some(Ordered::read(buf)));
        }
        if flags.parted {
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
            buf.put_u24_le(reliable_frame_index);
        }
        if let Some(seq_frame_index) = self.seq_frame_index {
            buf.put_u24_le(seq_frame_index);
        }
        if let Some(ordered) = self.ordered {
            ordered.write(buf);
        }
        if let Some(fragment) = self.fragment {
            fragment.write(buf);
        }
        buf.put(self.body);
    }

    /// Get the total size of this frame
    pub(crate) fn size(&self) -> usize {
        let mut size = self.flags.reliability.size();
        if self.fragment.is_some() {
            size += FRAGMENT_PART_SIZE;
        }
        size += self.body.remaining();
        size
    }
}

impl<B: Buf + Clone> Frame<B> {
    fn write_ref(&self, buf: &mut BytesMut) {
        self.flags.write(buf);
        // length in bits
        // self.body will be split up so cast to u16 should not overflow here
        debug_assert!(
            self.body.remaining() < (u16::MAX >> 3) as usize,
            "self.body should be constructed based on mtu"
        );
        buf.put_u16((self.body.remaining() << 3) as u16);
        if let Some(reliable_frame_index) = self.reliable_frame_index {
            buf.put_u24_le(reliable_frame_index);
        }
        if let Some(seq_frame_index) = self.seq_frame_index {
            buf.put_u24_le(seq_frame_index);
        }
        if let Some(ordered) = self.ordered {
            ordered.write(buf);
        }
        if let Some(fragment) = self.fragment {
            fragment.write(buf);
        }
        buf.put(self.body.clone()); // Bytes or BytesMut's clone are cheap
    }
}

/// Top 3 bits are reliability type, fourth bit is 1 when the frame is fragmented and part of a
/// compound.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Flags {
    raw: u8,
    pub(crate) reliability: Reliability,
    pub(crate) parted: bool,
    needs_bas: bool,
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

impl Flags {
    pub(crate) fn new(reliability: Reliability, parted: bool) -> Self {
        let mut raw = (reliability as u8) << 5;
        raw |= NEEDS_B_AND_AS_FLAG;
        if parted {
            raw |= PARTED_FLAG;
        }
        Self {
            raw,
            reliability,
            parted,
            needs_bas: true,
        }
    }

    fn read(buf: &mut BytesMut) -> Self {
        let raw = buf.get_u8();
        Self::parse(raw)
    }

    fn write(&self, buf: &mut BytesMut) {
        buf.put_u8(self.raw);
    }

    pub(crate) fn parse(raw: u8) -> Self {
        let r = raw >> 5;
        // Safety:
        // It is checked before transmute
        Self {
            raw,
            reliability: unsafe { std::mem::transmute::<u8, Reliability>(r) },
            parted: raw & PARTED_FLAG != 0,
            needs_bas: raw & NEEDS_B_AND_AS_FLAG != 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Ordered {
    pub(crate) frame_index: u24,
    pub(crate) channel: u8,
}

impl Ordered {
    fn read(buf: &mut BytesMut) -> Self {
        Self {
            frame_index: buf.get_u24_le(),
            channel: buf.get_u8(),
        }
    }

    fn write(self, buf: &mut BytesMut) {
        buf.put_u24_le(self.frame_index);
        buf.put_u8(self.channel);
    }
}

// The max number of addresses from a peer, constant here to avoid alloc heap memory
const MAX_SYSTEM_ADDRESSES_ENDPOINTS: usize = 20;

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
        system_addresses: [std::net::SocketAddr; MAX_SYSTEM_ADDRESSES_ENDPOINTS],
        request_timestamp: i64,
        accepted_timestamp: i64,
    },
    NewIncomingConnection {
        server_address: std::net::SocketAddr,
        system_addresses: [std::net::SocketAddr; MAX_SYSTEM_ADDRESSES_ENDPOINTS],
        request_timestamp: i64,
        accepted_timestamp: i64,
    },
    DisconnectNotification,
    DetectLostConnections,
    // User Packet
    User(Bytes),
}

impl std::fmt::Debug for FrameBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectedPing { .. } => write!(f, "ConnectedPing"),
            Self::ConnectedPong { .. } => write!(f, "ConnectedPong"),
            Self::ConnectionRequest { .. } => write!(f, "ConnectionRequest"),
            Self::ConnectionRequestAccepted { .. } => write!(f, "ConnectionRequestAccepted"),
            Self::NewIncomingConnection { .. } => write!(f, "NewIncomingConnection"),
            Self::DisconnectNotification => write!(f, "Disconnect"),
            Self::DetectLostConnections => write!(f, "DetectLostConnections"),
            Self::User(data) => write!(f, "User(size:{})", data.len()),
        }
    }
}

impl FrameBody {
    pub(crate) fn read(mut buf: Bytes) -> Result<Self, CodecError> {
        fn parse_system_addresses(buf: &mut Bytes) -> Result<[SocketAddr; 20], CodecError> {
            let mut addresses = [buf.get_socket_addr()?; MAX_SYSTEM_ADDRESSES_ENDPOINTS];
            #[allow(clippy::needless_range_loop)] // do not tech me
            for i in 1..MAX_SYSTEM_ADDRESSES_ENDPOINTS {
                if buf.remaining() > 16 {
                    addresses[i] = buf.get_socket_addr()?;
                    continue;
                }
                // early exit to parse `request_timestamp(8B)` and `accepted_timestamp(8B)`
                break;
            }
            if buf.remaining() < 16 {
                return Err(CodecError::InvalidPacketLength("frame body"));
            }
            debug_assert!(buf.remaining() == 16);
            Ok(addresses)
        }

        // checked in FrameSet, length is always greater than 0
        let Ok(id) = PackType::from_u8(buf.chunk()[0]) else {
            return Ok(Self::User(buf));
        };

        match id {
            PackType::ConnectedPing => Ok(Self::ConnectedPing {
                client_timestamp: read_buf!(buf, 9, {
                    buf.advance(1); // 1
                    buf.get_i64() // 8
                }),
            }),
            PackType::ConnectedPong => Ok(read_buf!(buf, 17, {
                buf.advance(1); // 1
                Self::ConnectedPong {
                    client_timestamp: buf.get_i64(), // 8,
                    server_timestamp: buf.get_i64(), // 8
                }
            })),
            PackType::ConnectionRequest => Ok(read_buf!(buf, 18, {
                buf.advance(1); // 1
                Self::ConnectionRequest {
                    client_guid: buf.get_u64(),        // 8
                    request_timestamp: buf.get_i64(),  // 8
                    use_encryption: buf.get_u8() != 0, // 1
                }
            })),
            PackType::ConnectionRequestAccepted => Ok(Self::ConnectionRequestAccepted {
                client_address: {
                    buf.advance(1);
                    buf.get_socket_addr()?
                },
                system_index: read_buf!(buf, 2, buf.get_u16()),
                system_addresses: parse_system_addresses(&mut buf)?,
                request_timestamp: buf.get_i64(),
                accepted_timestamp: buf.get_i64(),
            }),
            PackType::NewIncomingConnection => Ok(Self::NewIncomingConnection {
                server_address: {
                    buf.advance(1);
                    buf.get_socket_addr()?
                },
                system_addresses: parse_system_addresses(&mut buf)?,
                request_timestamp: buf.get_i64(),
                accepted_timestamp: buf.get_i64(),
            }),
            PackType::DisconnectNotification => Ok(Self::DisconnectNotification),
            PackType::DetectLostConnections => Ok(Self::DetectLostConnections),
            _ => Ok(Self::User(buf)),
        }
    }

    pub(crate) fn write(self, buf: &mut BytesMut) {
        match self {
            FrameBody::ConnectedPing { client_timestamp } => {
                buf.put_u8(PackType::ConnectedPing as u8);
                buf.put_i64(client_timestamp);
            }
            FrameBody::ConnectedPong {
                client_timestamp,
                server_timestamp,
            } => {
                buf.put_u8(PackType::ConnectedPong as u8);
                buf.put_i64(client_timestamp);
                buf.put_i64(server_timestamp);
            }
            FrameBody::ConnectionRequest {
                client_guid,
                request_timestamp,
                use_encryption,
            } => {
                buf.put_u8(PackType::ConnectionRequest as u8);
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
                buf.put_u8(PackType::ConnectionRequestAccepted as u8);
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
                buf.put_u8(PackType::NewIncomingConnection as u8);
                buf.put_socket_addr(server_address);
                for addr in system_addresses {
                    buf.put_socket_addr(addr);
                }
                buf.put_i64(request_timestamp);
                buf.put_i64(accepted_timestamp);
            }
            FrameBody::DisconnectNotification => {
                buf.put_u8(PackType::DisconnectNotification as u8);
            }
            FrameBody::DetectLostConnections => {
                buf.put_u8(PackType::DetectLostConnections as u8);
            }
            FrameBody::User(data) => {
                buf.put(data);
            }
        }
    }
}
