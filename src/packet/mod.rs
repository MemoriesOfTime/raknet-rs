pub(crate) mod connected;
pub(crate) mod unconnected;

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use bytes::{Buf, BufMut, BytesMut};

use crate::errors::CodecError;

#[macro_export]
macro_rules! read_buf {
    ($buf:expr, $len:expr, $exp:expr) => {{
        if $buf.remaining() < $len {
            return Err(CodecError::InvalidPacketLength);
        }
        $exp
    }};
}

/// Packet IDs. These packets play important role in raknet protocol.
/// Some of them appear at the first byte of a UDP data packet (like `UnconnectedPing1`), while
/// others are encapsulated in a `FrameSet` data packet and appear as the first byte of the body
/// (like `Game`).
#[derive(Debug)]
#[repr(u8)]
pub(crate) enum PackId {
    ConnectedPing = 0x00,
    UnconnectedPing1 = 0x01,
    UnconnectedPing2 = 0x02,
    ConnectedPong = 0x03,
    LostConnections = 0x04,
    OpenConnectionRequest1 = 0x05,
    OpenConnectionReply1 = 0x06,
    OpenConnectionRequest2 = 0x07,
    OpenConnectionReply2 = 0x08,
    ConnectionRequest = 0x09,
    ConnectionRequestAccepted = 0x10,
    AlreadyConnected = 0x12,
    NewIncomingConnection = 0x13,
    DisconnectNotification = 0x15,
    IncompatibleProtocolVersion = 0x19,
    UnconnectedPong = 0x1c,
    Game = 0xfe,

    /// The IDs of these three packets form a range, and only the one with the flag will be used
    /// here.
    Ack = 0xc0,
    Nack = 0xa0,
    FrameSet = 0x80,
}

impl PackId {
    pub(crate) fn from_u8(id: u8) -> Result<PackId, CodecError> {
        const ACK_FLAG: u8 = 0x40;
        const NACK_FLAG: u8 = 0x20;
        const FRAMESET_FLAG: u8 = 0x80;

        match id {
            0x00 => Ok(PackId::ConnectedPing),
            0x01 => Ok(PackId::UnconnectedPing1),
            0x02 => Ok(PackId::UnconnectedPing2),
            0x03 => Ok(PackId::ConnectedPong),
            0x04 => Ok(PackId::LostConnections),
            0x05 => Ok(PackId::OpenConnectionRequest1),
            0x06 => Ok(PackId::OpenConnectionReply1),
            0x07 => Ok(PackId::OpenConnectionRequest2),
            0x08 => Ok(PackId::OpenConnectionReply2),
            0x09 => Ok(PackId::ConnectionRequest),
            0x10 => Ok(PackId::ConnectionRequestAccepted),
            0x12 => Ok(PackId::AlreadyConnected),
            0x13 => Ok(PackId::NewIncomingConnection),
            0x15 => Ok(PackId::DisconnectNotification),
            0x19 => Ok(PackId::IncompatibleProtocolVersion),
            0x1c => Ok(PackId::UnconnectedPong),
            0xfe => Ok(PackId::Game),
            0xc0.. if id & ACK_FLAG != 0 => Ok(PackId::Ack),
            0xa0.. if id & NACK_FLAG != 0 => Ok(PackId::Nack),
            0x80.. if id & FRAMESET_FLAG != 0 => Ok(PackId::FrameSet),
            _ => Err(CodecError::InvalidPacketId(id)),
        }
    }

    /// Check if it is unconnected ping
    pub(crate) fn is_unconnected_ping(&self) -> bool {
        matches!(self, PackId::UnconnectedPing1 | PackId::UnconnectedPing2)
    }

    /// Check if it is a frame set packet
    pub(crate) fn is_frame_set(&self) -> bool {
        matches!(self, PackId::FrameSet)
    }

    /// Check if it is a ack packet
    pub(crate) fn is_ack(&self) -> bool {
        matches!(self, PackId::Ack)
    }

    /// Check if it is a nack packet
    pub(crate) fn is_nack(&self) -> bool {
        matches!(self, PackId::Nack)
    }
}

impl TryFrom<u8> for PackId {
    type Error = CodecError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::from_u8(value)
    }
}

impl From<PackId> for u8 {
    fn from(value: PackId) -> Self {
        value as u8
    }
}

/// Raknet packet
#[derive(Debug, PartialEq)]
pub(crate) enum Packet<T: Buf = BytesMut> {
    Unconnected(unconnected::Packet<T>),
    Connected(connected::Packet<T>),
}

impl Packet {
    /// Get the packet id
    pub(crate) fn pack_id(&self) -> PackId {
        match self {
            Packet::Unconnected(pack) => pack.pack_id(),
            Packet::Connected(pack) => pack.pack_id(),
        }
    }

    pub(crate) fn read(buf: &mut BytesMut) -> Result<Option<Self>, CodecError> {
        if buf.is_empty() {
            return Ok(None);
        }
        let pack_id: PackId = read_buf!(buf, 1, buf.get_u8().try_into()?);
        if pack_id.is_frame_set() {
            return Ok(Some(Self::Connected(connected::Packet::read_frame_set(
                buf,
            )?)));
        }
        if pack_id.is_ack() {
            return Ok(Some(Self::Connected(connected::Packet::read_ack(buf)?)));
        }
        if pack_id.is_nack() {
            return Ok(Some(Self::Connected(connected::Packet::read_nack(buf)?)));
        }
        match pack_id {
            PackId::UnconnectedPing1 | PackId::UnconnectedPing2 => {
                read_buf!(buf, 32, Ok(unconnected::Packet::read_unconnected_ping(buf)))
            }
            PackId::UnconnectedPong => {
                read_buf!(buf, 32, Ok(unconnected::Packet::read_unconnected_pong(buf)))
            }
            PackId::OpenConnectionRequest1 => {
                read_buf!(
                    buf,
                    19,
                    Ok(unconnected::Packet::read_open_connection_request1(buf))
                )
            }
            PackId::OpenConnectionReply1 => {
                read_buf!(
                    buf,
                    27,
                    Ok(unconnected::Packet::read_open_connection_reply1(buf))
                )
            }
            PackId::IncompatibleProtocolVersion => {
                read_buf!(
                    buf,
                    25,
                    Ok(unconnected::Packet::read_incompatible_protocol(buf))
                )
            }
            PackId::AlreadyConnected => {
                read_buf!(
                    buf,
                    24,
                    Ok(unconnected::Packet::read_already_connected(buf))
                )
            }
            PackId::OpenConnectionRequest2 => {
                unconnected::Packet::read_open_connection_request2(buf)
            }
            PackId::OpenConnectionReply2 => unconnected::Packet::read_open_connection_reply2(buf),
            _ => Err(CodecError::InvalidPacketId(pack_id.into())),
        }
        .map(|packet| Some(Self::Unconnected(packet)))
    }

    pub(crate) fn write(self, buf: &mut BytesMut) {
        buf.put_u8(self.pack_id().into());
        match self {
            Packet::Unconnected(packet) => {
                packet.write(buf);
            }
            Packet::Connected(packet) => {
                packet.write(buf);
            }
        }
    }
}

/// Magic sequence is a sequence of bytes which is found in every unconnected message sent in Raknet
pub(crate) const MAGIC: [u8; 16] = [
    0x00, 0xff, 0xff, 0x00, 0xfe, 0xfe, 0xfe, 0xfe, 0xfd, 0xfd, 0xfd, 0xfd, 0x12, 0x34, 0x56, 0x78,
];

pub(crate) trait MagicRead: private::SealedBuf {
    /// Get the raknet magic and return a bool if it matches the magic
    fn get_checked_magic(&mut self) -> bool;

    /// Sometimes we do not care about how the magic matches
    fn get_unchecked_magic(&mut self);
}

pub(crate) trait MagicWrite: private::SealedBufMut {
    /// Put the raknet magic
    fn put_magic(&mut self);
}

impl<T: BufMut> private::SealedBufMut for T {}
impl<T: Buf> private::SealedBuf for T {}

impl<T: Buf> MagicRead for T {
    #[allow(clippy::needless_range_loop)]
    fn get_checked_magic(&mut self) -> bool {
        let mut matches = true;
        for i in 0..MAGIC.len() {
            let byte = self.chunk()[i];
            if byte != MAGIC[i] {
                matches = false;
            }
        }
        self.advance(MAGIC.len());
        matches
    }

    fn get_unchecked_magic(&mut self) {
        self.advance(MAGIC.len());
    }
}

impl<T: BufMut> MagicWrite for T {
    fn put_magic(&mut self) {
        self.put_slice(&MAGIC);
    }
}

pub(crate) trait SocketAddrRead: private::SealedBuf {
    fn get_socket_addr(&mut self) -> Result<SocketAddr, CodecError>;
}

pub(crate) trait SocketAddrWrite: private::SealedBufMut {
    fn put_socket_addr(&mut self, addr: SocketAddr);
}

impl<T: Buf> SocketAddrRead for T {
    fn get_socket_addr(&mut self) -> Result<SocketAddr, CodecError> {
        let ver = read_buf!(self, 1, self.get_u8());
        match ver {
            4 => {
                read_buf!(self, 6, {
                    let ip = Ipv4Addr::from_bits(self.get_u32());
                    let port = self.get_u16();
                    Ok(SocketAddr::V4(SocketAddrV4::new(ip, port)))
                })
            }
            6 => {
                // TODO: to be determined
                read_buf!(self, 28, {
                    let family = self.get_u16();
                    if family != 0x17 {
                        return Err(CodecError::InvalidIPV6Family(family));
                    }
                    let port = self.get_u16();
                    let flow_info = self.get_u32();
                    let ip = Ipv6Addr::from_bits(self.get_u128());
                    let scope_ip = self.get_u32();
                    Ok(SocketAddr::V6(SocketAddrV6::new(
                        ip, port, flow_info, scope_ip,
                    )))
                })
            }
            _ => Err(CodecError::InvalidIPVer(ver)),
        }
    }
}

impl<T: BufMut> SocketAddrWrite for T {
    fn put_socket_addr(&mut self, addr: SocketAddr) {
        match addr {
            SocketAddr::V4(v4) => {
                self.put_u8(4);
                self.put_slice(&v4.ip().octets());
                self.put_u16(v4.port());
            }
            SocketAddr::V6(v6) => {
                self.put_u8(6);
                self.put_u16(0x17);
                self.put_u16(v6.port());
                self.put_u32(v6.flowinfo());
                self.put_slice(&v6.ip().octets());
                self.put_u32(v6.scope_id());
            }
        }
    }
}

mod private {
    pub(crate) trait SealedBuf {}
    pub(crate) trait SealedBufMut {}
}
