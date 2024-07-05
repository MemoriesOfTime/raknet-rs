pub(crate) mod connected;
pub(crate) mod unconnected;

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use bytes::{Buf, BufMut, BytesMut};
use connected::FramesMut;

use self::connected::Frames;
use crate::errors::CodecError;

macro_rules! read_buf {
    ($buf:expr, $len:expr, $exp:expr) => {{
        if $buf.remaining() < $len {
            return Err(CodecError::InvalidPacketLength("particular sized pack"));
        }
        $exp
    }};
}

pub(in crate::packet) use read_buf;

const VALID_FLAG: u8 = 0b1000_0000;
const ACK_FLAG: u8 = 0b1100_0000;
const NACK_FLAG: u8 = 0b1010_0000;

const PARTED_FLAG: u8 = 0b0001_0000;
const CONTINUOUS_SEND_FLAG: u8 = 0b0000_1000;
const NEEDS_B_AND_AS_FLAG: u8 = 0b0000_0100;

// 1B ID + 3B seq num
pub(crate) const FRAME_SET_HEADER_SIZE: usize = 4;

// u32 + u16 + u32
pub(crate) const FRAGMENT_PART_SIZE: usize = 10;

/// Packet Types. These packets play important role in raknet protocol.
/// Some of them appear at the first byte of a UDP data packet (like `UnconnectedPing1`), while
/// others are encapsulated in a `FrameSet` data packet and appear as the first byte of the body
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[allow(dead_code)] // may used in future
pub(crate) enum PackType {
    ConnectedPing = 0x00,
    UnconnectedPing1 = 0x01,
    UnconnectedPing2 = 0x02,
    ConnectedPong = 0x03,
    DetectLostConnections = 0x04,
    OpenConnectionRequest1 = 0x05,
    OpenConnectionReply1 = 0x06,
    OpenConnectionRequest2 = 0x07,
    OpenConnectionReply2 = 0x08,
    ConnectionRequest = 0x09,
    ConnectionRequestAccepted = 0x10,
    ConnectionRequestFailed = 0x11,
    AlreadyConnected = 0x12,
    NewIncomingConnection = 0x13,
    NoFreeIncomingConnections = 0x14,
    DisconnectNotification = 0x15,
    ConnectionLost = 0x16,
    ConnectionBanned = 0x17,
    IncompatibleProtocolVersion = 0x19,
    IpRecentlyConnected = 0x1a,
    Timestamp = 0x1b,
    UnconnectedPong = 0x1c,
    AdvertiseSystem = 0x1d,

    /// The types of these three packets form a range, and only the one with the flag will be used
    /// here.
    Ack = ACK_FLAG,
    Nack = NACK_FLAG,
    FrameSet = VALID_FLAG,
}

impl PackType {
    pub(crate) fn from_u8(id: u8) -> Result<PackType, CodecError> {
        match id {
            0x00 => Ok(PackType::ConnectedPing),
            0x01 => Ok(PackType::UnconnectedPing1),
            0x02 => Ok(PackType::UnconnectedPing2),
            0x03 => Ok(PackType::ConnectedPong),
            0x04 => Ok(PackType::DetectLostConnections),
            0x05 => Ok(PackType::OpenConnectionRequest1),
            0x06 => Ok(PackType::OpenConnectionReply1),
            0x07 => Ok(PackType::OpenConnectionRequest2),
            0x08 => Ok(PackType::OpenConnectionReply2),
            0x09 => Ok(PackType::ConnectionRequest),
            0x10 => Ok(PackType::ConnectionRequestAccepted),
            0x11 => Ok(PackType::ConnectionRequestFailed),
            0x12 => Ok(PackType::AlreadyConnected),
            0x13 => Ok(PackType::NewIncomingConnection),
            0x14 => Ok(PackType::NoFreeIncomingConnections),
            0x15 => Ok(PackType::DisconnectNotification),
            0x16 => Ok(PackType::ConnectionLost),
            0x17 => Ok(PackType::ConnectionBanned),
            0x19 => Ok(PackType::IncompatibleProtocolVersion),
            0x1a => Ok(PackType::IpRecentlyConnected),
            0x1b => Ok(PackType::Timestamp),
            0x1c => Ok(PackType::UnconnectedPong),
            0x1d => Ok(PackType::AdvertiseSystem),
            ACK_FLAG.. => Ok(PackType::Ack),
            NACK_FLAG.. => Ok(PackType::Nack),
            VALID_FLAG.. => Ok(PackType::FrameSet),
            _ => Err(CodecError::InvalidPacketType(id)),
        }
    }

    /// Check if it is a frame set packet
    pub(crate) fn is_frame_set(&self) -> bool {
        matches!(self, PackType::FrameSet)
    }

    /// Check if it is a ack packet
    pub(crate) fn is_ack(&self) -> bool {
        matches!(self, PackType::Ack)
    }

    /// Check if it is a nack packet
    pub(crate) fn is_nack(&self) -> bool {
        matches!(self, PackType::Nack)
    }
}

impl TryFrom<u8> for PackType {
    type Error = CodecError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::from_u8(value)
    }
}

impl From<PackType> for u8 {
    fn from(value: PackType) -> Self {
        value as u8
    }
}

/// Raknet packet
#[derive(Debug, PartialEq, Clone)]
pub(crate) enum Packet<S> {
    Unconnected(unconnected::Packet),
    Connected(connected::Packet<S>),
}

impl<B: Buf> Packet<Frames<B>> {
    pub(crate) fn write(self, buf: &mut BytesMut) {
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

impl Packet<FramesMut> {
    pub(crate) fn read(buf: &mut BytesMut) -> Result<Option<Self>, CodecError> {
        if buf.is_empty() {
            return Ok(None);
        }
        // read more
        if buf.chunk()[0] == 0 {
            buf.clear();
            return Ok(None);
        }

        let pack_type: PackType = read_buf!(buf, 1, buf.get_u8().try_into()?);
        if pack_type.is_frame_set() {
            return Ok(Some(Self::Connected(connected::Packet::read_frame_set(
                buf,
            )?)));
        }
        if pack_type.is_ack() {
            return Ok(Some(Self::Connected(connected::Packet::read_ack(buf)?)));
        }
        if pack_type.is_nack() {
            return Ok(Some(Self::Connected(connected::Packet::read_nack(buf)?)));
        }
        match pack_type {
            PackType::UnconnectedPing1 | PackType::UnconnectedPing2 => {
                read_buf!(buf, 32, unconnected::Packet::read_unconnected_ping(buf))
            }
            PackType::UnconnectedPong => {
                read_buf!(buf, 34, unconnected::Packet::read_unconnected_pong(buf))
            }
            PackType::OpenConnectionRequest1 => {
                read_buf!(
                    buf,
                    19,
                    unconnected::Packet::read_open_connection_request1(buf)
                )
            }
            PackType::OpenConnectionReply1 => {
                read_buf!(
                    buf,
                    27,
                    unconnected::Packet::read_open_connection_reply1(buf)
                )
            }
            PackType::IncompatibleProtocolVersion => {
                read_buf!(
                    buf,
                    25,
                    unconnected::Packet::read_incompatible_protocol(buf)
                )
            }
            PackType::AlreadyConnected => {
                read_buf!(buf, 24, unconnected::Packet::read_already_connected(buf))
            }
            PackType::ConnectionRequestFailed => {
                read_buf!(
                    buf,
                    24,
                    unconnected::Packet::read_connection_request_failed(buf)
                )
            }
            PackType::OpenConnectionRequest2 => {
                unconnected::Packet::read_open_connection_request2(buf)
            }
            PackType::OpenConnectionReply2 => unconnected::Packet::read_open_connection_reply2(buf),
            _ => Err(CodecError::InvalidPacketType(pack_type.into())),
        }
        .map(|packet| Some(Self::Unconnected(packet)))
    }
}

/// Magic sequence is a sequence of bytes which is found in every unconnected message sent in Raknet
pub(crate) const MAGIC: [u8; 16] = [
    0x00, 0xff, 0xff, 0x00, 0xfe, 0xfe, 0xfe, 0xfe, 0xfd, 0xfd, 0xfd, 0xfd, 0x12, 0x34, 0x56, 0x78,
];

pub(crate) trait MagicRead {
    /// Get the raknet magic and return a bool if it matches the magic
    fn get_checked_magic(&mut self) -> Result<(), CodecError>;
}

pub(crate) trait MagicWrite {
    /// Put the raknet magic
    fn put_magic(&mut self);
}

impl<B: Buf> MagicRead for B {
    #![allow(clippy::needless_range_loop)]
    fn get_checked_magic(&mut self) -> Result<(), CodecError> {
        for i in 0..MAGIC.len() {
            let byte = self.chunk()[i];
            if byte != MAGIC[i] {
                return Err(CodecError::MagicNotMatched(i, byte));
            }
        }
        self.advance(MAGIC.len());
        Ok(())
    }
}

impl<B: BufMut> MagicWrite for B {
    fn put_magic(&mut self) {
        self.put_slice(&MAGIC);
    }
}

pub(crate) trait SocketAddrRead {
    fn get_socket_addr(&mut self) -> Result<SocketAddr, CodecError>;
}

pub(crate) trait SocketAddrWrite {
    fn put_socket_addr(&mut self, addr: SocketAddr);
}

impl<B: Buf> SocketAddrRead for B {
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

impl<B: BufMut> SocketAddrWrite for B {
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
