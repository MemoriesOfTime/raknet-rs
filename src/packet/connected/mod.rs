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
                if frame.set[0].flags.parted {
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

impl Packet<FramesMut> {
    pub(super) fn read_frame_set(buf: &mut BytesMut) -> Result<Self, CodecError> {
        Ok(Packet::FrameSet(FrameSet::read(buf)?))
    }
}
