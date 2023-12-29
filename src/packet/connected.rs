use std::fmt::Display;
use std::hash::{Hash, Hasher};

use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::errors::CodecError;
use crate::packet::PackId;
use crate::read_buf;

// Packet when RakNet has established a connection
#[derive(Debug, PartialEq, Clone)]
pub(crate) enum Packet<B: Buf> {
    FrameSet(FrameSet<B>),
    Ack(AckOrNack),
    Nack(AckOrNack),
}

#[derive(Debug, PartialEq, Clone)]
pub(crate) struct FrameSet<B: Buf> {
    pub(crate) seq_num: Uint24le,
    pub(crate) frames: Vec<Frame<B>>,
}

#[derive(Eq, PartialEq, Clone)]
pub(crate) struct Frame<B: Buf> {
    pub(crate) flags: Flags,
    pub(crate) reliable_frame_index: Option<Uint24le>,
    pub(crate) seq_frame_index: Option<Uint24le>,
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
            .field("body_size", &self.body.remaining())
            .finish()
    }
}

impl<B: Buf> Hash for Frame<B> {
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
        self.body.chunk().hash(state);
    }
}

impl Frame<BytesMut> {
    pub(crate) fn freeze(self) -> Frame<Bytes> {
        Frame {
            body: self.body.freeze(),
            ..self
        }
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
        Ok(Frame {
            flags,
            reliable_frame_index,
            seq_frame_index,
            ordered,
            fragment,
            body: read_buf!(buf, length, buf.split_to(length)),
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

impl FrameSet<BytesMut> {
    fn read(buf: &mut BytesMut) -> Result<Self, CodecError> {
        // TODO: get a proper const for every scenario
        const AVG_FRAME_SIZE: usize = 30;

        let seq_num = read_buf!(buf, 3, Uint24le::read(buf));
        // I just want to avoid reallocate :)
        let mut frames = Vec::with_capacity(buf.remaining() / AVG_FRAME_SIZE);
        while buf.has_remaining() {
            frames.push(Frame::read(buf)?);
        }
        if frames.is_empty() {
            return Err(CodecError::InvalidPacketLength("frame set"));
        }
        Ok(FrameSet { seq_num, frames })
    }
}

impl<B: Buf> FrameSet<B> {
    /// Get the inner packet id
    pub(crate) fn inner_pack_id(&self) -> Result<PackId, CodecError> {
        PackId::from_u8(
            *self.frames[0]
                .body
                .chunk()
                .first()
                .ok_or(CodecError::InvalidPacketLength("frame set inner pack id"))?,
        )
    }

    fn write(self, buf: &mut BytesMut) {
        self.seq_num.write(buf);
        for frame in self.frames {
            frame.write(buf);
        }
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

/// Top 3 bits are reliability type, fourth bit is 1 when the frame is fragmented and part of a
/// compound.
#[derive(Debug, Eq, PartialEq, Clone)]
pub(crate) struct Flags {
    raw: u8,
    reliability: Reliability,
    parted: bool,
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
        const PARTED_FLAG: u8 = 0b0001_0000;

        let r = raw >> 5;
        // Safety:
        // It is checked before transmute
        Self {
            raw,
            reliability: unsafe { std::mem::transmute(r) },
            parted: raw & PARTED_FLAG != 0,
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

#[derive(Debug, PartialEq, Clone)]
pub(crate) struct AckOrNack {
    records: Vec<Record>,
}

impl AckOrNack {
    /// Extend a packet from a sorted sequence numbers iterator based on mtu.
    /// Notice that a uint24le must be unique in the whole iterator
    pub(crate) fn extend_from<I: Iterator<Item = Uint24le>>(
        mut sorted_seq_nums: I,
        mut mtu: u16,
    ) -> Option<Self> {
        // TODO: get a proper const for every scenario
        const SEQ_NUM_SIZE_TO_RECORD_DIV_CONST: usize = 10;

        // pack_id(1) + length(2) + single record(4) = 7
        debug_assert!(mtu >= 7, "7 is the least size of mtu");

        let Some(mut first) = sorted_seq_nums.next() else {
            return None;
        };

        // Emm, I don't know how to get the exact capacity of Vec, I just do not want
        // to reallocate when push records
        let (low, upper_maybe) = sorted_seq_nums.size_hint();
        let mut records = upper_maybe.map_or_else(
            || Vec::with_capacity(low / SEQ_NUM_SIZE_TO_RECORD_DIV_CONST),
            |upper| Vec::with_capacity(upper / SEQ_NUM_SIZE_TO_RECORD_DIV_CONST),
        );

        let mut last = first;
        let mut upgrade_flag = true;
        // first byte is pack_id, next 2 bytes are length, the first seq_num takes at least 4 bytes
        mtu -= 7;
        loop {
            // we cannot poll sorted_seq_nums because 4 is the least size of a record
            if mtu < 4 {
                break;
            }
            let Some(seq_num) = sorted_seq_nums.next() else {
                break;
            };
            if seq_num.0 == last.0 + 1 {
                if upgrade_flag {
                    mtu -= 3;
                    upgrade_flag = false;
                }
                last = seq_num;
                continue;
            }
            mtu -= 4;
            upgrade_flag = true;
            if first.0 != last.0 {
                records.push(Record::Range(first, last));
            } else {
                records.push(Record::Single(first));
            }
            first = seq_num;
            last = seq_num;
        }

        if first.0 != last.0 {
            records.push(Record::Range(first, last));
        } else {
            records.push(Record::Single(first));
        }

        Some(Self { records })
    }

    fn read(buf: &mut BytesMut) -> Result<Self, CodecError> {
        const MAX_ACKNOWLEDGEMENT_PACKETS: u32 = 8192;

        let mut ack_cnt = 0;
        let record_cnt = buf.get_u16();
        let mut records = Vec::with_capacity(record_cnt as usize);
        for _ in 0..record_cnt {
            let record = Record::read(buf)?;
            ack_cnt += record.ack_cnt();
            if ack_cnt > MAX_ACKNOWLEDGEMENT_PACKETS {
                return Err(CodecError::AckCountExceed);
            }
            records.push(record);
        }
        Ok(Self { records })
    }

    fn write(self, buf: &mut BytesMut) {
        debug_assert!(
            self.records.len() < u16::MAX as usize,
            "self.records should be constructed based on mtu"
        );
        buf.put_u16(self.records.len() as u16);
        for record in self.records {
            record.write(buf);
        }
    }
}

const RECORD_RANGE: u8 = 0;
const RECORD_SINGLE: u8 = 1;

#[derive(Debug, PartialEq, Clone)]
pub(crate) enum Record {
    Range(Uint24le, Uint24le),
    Single(Uint24le),
}

impl Record {
    fn read(buf: &mut BytesMut) -> Result<Self, CodecError> {
        let record_type = read_buf!(buf, 1, buf.get_u8());
        match record_type {
            RECORD_RANGE => read_buf!(
                buf,
                6,
                Ok(Record::Range(Uint24le::read(buf), Uint24le::read(buf)))
            ),
            RECORD_SINGLE => read_buf!(buf, 3, Ok(Record::Single(Uint24le::read(buf)))),
            _ => Err(CodecError::InvalidRecordType(record_type)),
        }
    }

    fn write(self, buf: &mut BytesMut) {
        match self {
            Record::Range(start, end) => {
                buf.put_u8(RECORD_RANGE);
                start.write(buf);
                end.write(buf);
            }
            Record::Single(idx) => {
                buf.put_u8(RECORD_SINGLE);
                idx.write(buf);
            }
        }
    }

    fn ack_cnt(&self) -> u32 {
        match self {
            Record::Range(start, end) => end.0 - start.0 + 1,
            Record::Single(_) => 1,
        }
    }
}

impl<B: Buf> Packet<B> {
    pub(super) fn pack_id(&self) -> PackId {
        match self {
            Packet::FrameSet(_) => PackId::FrameSet,
            Packet::Ack(_) => PackId::Ack,
            Packet::Nack(_) => PackId::Nack,
        }
    }

    pub(super) fn read_ack(buf: &mut BytesMut) -> Result<Self, CodecError> {
        Ok(Packet::Ack(AckOrNack::read(buf)?))
    }

    pub(super) fn read_nack(buf: &mut BytesMut) -> Result<Self, CodecError> {
        Ok(Packet::Nack(AckOrNack::read(buf)?))
    }

    pub(super) fn write(self, buf: &mut BytesMut) {
        match self {
            Packet::FrameSet(frame) => frame.write(buf),
            Packet::Ack(ack) | Packet::Nack(ack) => ack.write(buf),
        }
    }
}

impl Packet<BytesMut> {
    pub(super) fn read_frame_set(buf: &mut BytesMut) -> Result<Self, CodecError> {
        Ok(Packet::FrameSet(FrameSet::read(buf)?))
    }
}

// enum BodyPacket {
//     ConnectedPing {
//         client_timestamp: i64,
//     },
//     ConnectedPong {
//         client_timestamp: i64,
//         server_timestamp: i64,
//     },
//     ConnectionRequest {
//         client_guid: u64,
//         request_timestamp: i64,
//         use_encryption: bool,
//     },
//     ConnectionRequestAccepted {
//         client_address: std::net::SocketAddr,
//         // system_index: u16,
//         system_addresses: [std::net::SocketAddr; 10],
//         request_timestamp: i64,
//         accepted_timestamp: i64,
//     },
//     NewIncomingConnection {
//         server_address: std::net::SocketAddr,
//         system_addresses: [std::net::SocketAddr; 10],
//         request_timestamp: i64,
//         accepted_timestamp: i64,
//     },
//     Disconnect,
//     Game,
// }

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_ack_should_not_overflow_mtu() {
        let mtu: u16 = 21;
        let mut buf = BytesMut::with_capacity(mtu as usize);

        let test_cases = [
            // 3 + 0-2(7) + 4-5(7) + 7(4) = 21, remain 8
            (vec![0, 1, 2, 4, 5, 7, 8], 21, 1),
            // 3 + 0-1(7) + 3-4(7) + 6(4) = 21, remain 7, 9
            (vec![0, 1, 3, 4, 6, 7, 9], 21, 2),
            // 3 + 0(4) + 2(4) + 4(4) + 6(4) = 19, remain 8, 10, 12
            (vec![0, 2, 4, 6, 8, 10, 12], 19, 3),
            // 3 + 0(4) + 2(4) + 5-6(7) = 18, remain 8, 9, 12
            (vec![0, 2, 5, 6, 8, 9, 12], 18, 3),
            // 3 + 0-1(7) = 10, no remain
            (vec![0, 1], 10, 0),
            // 3 + 0(4) + 2-3(7) = 14, no remain
            (vec![0, 2, 3], 14, 0),
            // 3 + 0(4) + 2(4) + 4(4) = 15, no remain
            (vec![0, 2, 4], 15, 0),
        ];
        for (seq_nums, len, remain) in test_cases {
            buf.clear();
            // pack id
            buf.put_u8(0);
            let mut seq_nums = seq_nums.into_iter().map(Uint24le);
            let ack = AckOrNack::extend_from(&mut seq_nums, mtu).unwrap();
            ack.write(&mut buf);
            assert_eq!(buf.len(), len);
            assert_eq!(seq_nums.len(), remain);
        }
    }
}
