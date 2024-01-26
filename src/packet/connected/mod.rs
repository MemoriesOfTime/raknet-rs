use std::fmt::Display;
use std::hash::Hash;

use bytes::{Buf, BufMut, BytesMut};

use crate::errors::CodecError;
use crate::packet::PackType;

mod ack;
mod frame_set;

pub(crate) use ack::*;
pub(crate) use frame_set::*;

use super::{ACK_FLAG, CONTINUOUS_SEND_FLAG, NACK_FLAG, NEEDS_B_AND_AS_FLAG, VALID_FLAG};

// Packet when RakNet has established a connection
#[derive(Debug, PartialEq, Clone)]
pub(crate) enum Packet<S> {
    FrameSet(FrameSet<S>),
    Ack(AckOrNack),
    Nack(AckOrNack),
}

impl<S> Packet<S> {
    pub(crate) fn pack_type(&self) -> PackType {
        match self {
            Packet::FrameSet(_) => PackType::FrameSet,
            Packet::Ack(_) => PackType::Ack,
            Packet::Nack(_) => PackType::Nack,
        }
    }

    pub(super) fn read_ack(buf: &mut BytesMut) -> Result<Self, CodecError> {
        Ok(Packet::Ack(AckOrNack::read(buf)?))
    }

    pub(super) fn read_nack(buf: &mut BytesMut) -> Result<Self, CodecError> {
        Ok(Packet::Nack(AckOrNack::read(buf)?))
    }
}

impl<B: Buf> Packet<Frames<B>> {
    pub(super) fn write(self, buf: &mut BytesMut) {
        match self {
            Packet::FrameSet(frame) => {
                let mut flag = VALID_FLAG | NEEDS_B_AND_AS_FLAG;
                if frame.set[0].flags.parted() {
                    flag |= CONTINUOUS_SEND_FLAG;
                }
                buf.put_u8(flag);
                frame.write(buf);
            }
            Packet::Ack(ack) => {
                buf.put_u8(ACK_FLAG);
                ack.write(buf);
            }
            Packet::Nack(ack) => {
                buf.put_u8(NACK_FLAG);
                ack.write(buf);
            }
        }
    }
}

impl<B: Buf> Packet<Frame<B>> {
    pub(super) fn write(self, buf: &mut BytesMut) {
        match self {
            Packet::FrameSet(frame) => {
                let flag = VALID_FLAG | NEEDS_B_AND_AS_FLAG;
                buf.put_u8(flag);
                frame.write(buf);
            }
            Packet::Ack(ack) => {
                buf.put_u8(ACK_FLAG);
                ack.write(buf);
            }
            Packet::Nack(ack) => {
                buf.put_u8(NACK_FLAG);
                ack.write(buf);
            }
        }
    }
}

impl Packet<Frames<BytesMut>> {
    pub(super) fn read_frame_set(buf: &mut BytesMut) -> Result<Self, CodecError> {
        Ok(Packet::FrameSet(FrameSet::read(buf)?))
    }
}

/// `uint24` little-endian but actually occupies 4 bytes.
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub(crate) struct Uint24le(pub u32);

impl Uint24le {
    fn read(buf: &mut BytesMut) -> Self {
        // safe cast because only 3 bytes will not overflow
        Self(buf.get_uint_le(3) as u32)
    }

    fn write(self, buf: &mut BytesMut) {
        buf.put_uint_le(self.0 as u64, 3);
    }
}

impl Display for Uint24le {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
