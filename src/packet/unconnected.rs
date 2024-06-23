use std::net::SocketAddr;

use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::errors::CodecError;
use crate::packet::{read_buf, MagicRead, MagicWrite, PackType, SocketAddrRead, SocketAddrWrite};

/// Request sent before establishing a connection
#[derive(Debug, PartialEq, Clone)]
pub(crate) enum Packet {
    UnconnectedPing {
        send_timestamp: i64,
        magic: (),
        client_guid: u64,
    },
    UnconnectedPong {
        send_timestamp: i64,
        server_guid: u64,
        magic: (),
        data: Bytes,
    },
    OpenConnectionRequest1 {
        magic: (),
        protocol_version: u8,
        mtu: u16,
    },
    OpenConnectionReply1 {
        magic: (),
        server_guid: u64,
        use_encryption: bool,
        mtu: u16,
    },
    OpenConnectionRequest2 {
        magic: (),
        server_address: SocketAddr,
        mtu: u16,
        client_guid: u64,
    },
    OpenConnectionReply2 {
        magic: (),
        server_guid: u64,
        client_address: SocketAddr,
        mtu: u16,
        encryption_enabled: bool,
    },
    IncompatibleProtocol {
        server_protocol: u8,
        magic: (),
        server_guid: u64,
    },
    AlreadyConnected {
        magic: (),
        server_guid: u64,
    },
    ConnectionRequestFailed {
        magic: (),
        server_guid: u64,
    },
}

impl Packet {
    pub(crate) fn pack_type(&self) -> PackType {
        match self {
            Packet::UnconnectedPing { .. } => {
                // > [Wiki](https://wiki.vg/Raknet_Protocol) said:
                // > 0x02 is only replied to if there are open connections to the server.
                // It is in unconnected mod, so we just return UnconnectedPing1
                PackType::UnconnectedPing1
            }
            Packet::UnconnectedPong { .. } => PackType::UnconnectedPong,
            Packet::OpenConnectionRequest1 { .. } => PackType::OpenConnectionRequest1,
            Packet::OpenConnectionReply1 { .. } => PackType::OpenConnectionReply1,
            Packet::OpenConnectionRequest2 { .. } => PackType::OpenConnectionRequest2,
            Packet::OpenConnectionReply2 { .. } => PackType::OpenConnectionReply2,
            Packet::IncompatibleProtocol { .. } => PackType::IncompatibleProtocolVersion,
            Packet::AlreadyConnected { .. } => PackType::AlreadyConnected,
            Packet::ConnectionRequestFailed { .. } => PackType::ConnectionRequestFailed,
        }
    }

    pub(super) fn read_unconnected_ping(buf: &mut BytesMut) -> Result<Self, CodecError> {
        Ok(Packet::UnconnectedPing {
            send_timestamp: buf.get_i64(),   // 8
            magic: buf.get_checked_magic()?, // 16
            client_guid: buf.get_u64(),      // 8
        })
    }

    pub(super) fn read_unconnected_pong(buf: &mut BytesMut) -> Result<Self, CodecError> {
        Ok(Packet::UnconnectedPong {
            send_timestamp: buf.get_i64(),   // 8
            server_guid: buf.get_u64(),      // 8
            magic: buf.get_checked_magic()?, // 16
            data: {
                let len = buf.get_u16();
                buf.split_off(len as usize).freeze()
            }, // > 2
        })
    }

    pub(super) fn read_open_connection_request1(buf: &mut BytesMut) -> Result<Self, CodecError> {
        Ok(Packet::OpenConnectionRequest1 {
            magic: buf.get_checked_magic()?, // 16
            protocol_version: buf.get_u8(),  // 1
            mtu: buf.get_u16(),              // 2
        })
    }

    pub(super) fn read_open_connection_reply1(buf: &mut BytesMut) -> Result<Self, CodecError> {
        Ok(Packet::OpenConnectionReply1 {
            magic: buf.get_checked_magic()?,   // 16
            server_guid: buf.get_u64(),        // 8
            use_encryption: buf.get_u8() != 0, // 1
            mtu: buf.get_u16(),                // 2
        })
    }

    pub(super) fn read_open_connection_request2(buf: &mut BytesMut) -> Result<Self, CodecError> {
        Ok(Packet::OpenConnectionRequest2 {
            magic: read_buf!(buf, 16, buf.get_checked_magic())?,
            server_address: buf.get_socket_addr()?,
            mtu: read_buf!(buf, 2, buf.get_u16()),
            client_guid: read_buf!(buf, 8, buf.get_u64()),
        })
    }

    pub(super) fn read_open_connection_reply2(buf: &mut BytesMut) -> Result<Self, CodecError> {
        Ok(Packet::OpenConnectionReply2 {
            magic: read_buf!(buf, 16, buf.get_checked_magic())?,
            server_guid: read_buf!(buf, 8, buf.get_u64()),
            client_address: buf.get_socket_addr()?,
            mtu: read_buf!(buf, 2, buf.get_u16()),
            encryption_enabled: read_buf!(buf, 1, buf.get_u8() != 0),
        })
    }

    pub(super) fn read_incompatible_protocol(buf: &mut BytesMut) -> Result<Self, CodecError> {
        Ok(Packet::IncompatibleProtocol {
            server_protocol: buf.get_u8(),   // 1
            magic: buf.get_checked_magic()?, // 16
            server_guid: buf.get_u64(),      // 8
        })
    }

    pub(super) fn read_already_connected(buf: &mut BytesMut) -> Result<Self, CodecError> {
        Ok(Packet::AlreadyConnected {
            magic: buf.get_checked_magic()?, // 16
            server_guid: buf.get_u64(),      // 8
        })
    }

    pub(super) fn read_connection_request_failed(buf: &mut BytesMut) -> Result<Self, CodecError> {
        Ok(Packet::ConnectionRequestFailed {
            magic: buf.get_checked_magic()?, // 16
            server_guid: buf.get_u64(),      // 8
        })
    }

    pub(super) fn write(self, buf: &mut BytesMut) {
        // Fixed id (type)
        buf.put_u8(self.pack_type().into());
        match self {
            Packet::UnconnectedPing {
                send_timestamp,
                magic: _magic,
                client_guid,
            } => {
                buf.put_i64(send_timestamp);
                buf.put_magic();
                buf.put_u64(client_guid);
            }
            Packet::UnconnectedPong {
                send_timestamp,
                server_guid,
                magic: _magic,
                data,
            } => {
                buf.put_i64(send_timestamp);
                buf.put_u64(server_guid);
                buf.put_magic();
                buf.put_u16(data.len() as u16);
                buf.put(data);
            }
            Packet::OpenConnectionRequest1 {
                magic: _magic,
                protocol_version,
                mtu,
            } => {
                buf.put_magic();
                buf.put_u8(protocol_version);
                buf.put_u16(mtu);
            }
            Packet::OpenConnectionReply1 {
                magic: _magic,
                server_guid,
                use_encryption: _use_encryption,
                mtu,
            } => {
                buf.put_magic();
                buf.put_u64(server_guid);
                buf.put_u8(0);
                buf.put_u16(mtu);
            }
            Packet::OpenConnectionRequest2 {
                magic: _magic,
                server_address,
                mtu,
                client_guid,
            } => {
                buf.put_magic();
                buf.put_socket_addr(server_address);
                buf.put_u16(mtu);
                buf.put_u64(client_guid);
            }
            Packet::OpenConnectionReply2 {
                magic: _magic,
                server_guid,
                client_address,
                mtu,
                encryption_enabled: _encryption_enabled,
            } => {
                buf.put_magic();
                buf.put_u64(server_guid);
                buf.put_socket_addr(client_address);
                buf.put_u16(mtu);
                buf.put_u8(0);
            }
            Packet::IncompatibleProtocol {
                server_protocol,
                magic: _magic,
                server_guid,
            } => {
                buf.put_u8(server_protocol);
                buf.put_magic();
                buf.put_u64(server_guid);
            }
            Packet::AlreadyConnected {
                magic: _magic,
                server_guid,
            } => {
                buf.put_magic();
                buf.put_u64(server_guid);
            }
            Packet::ConnectionRequestFailed {
                magic: _magic,
                server_guid,
            } => {
                buf.put_magic();
                buf.put_u64(server_guid);
            }
        }
    }
}
