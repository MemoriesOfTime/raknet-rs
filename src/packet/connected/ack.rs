use bytes::{Buf, BufMut, BytesMut};

use super::Uint24le;
use crate::errors::CodecError;
use crate::read_buf;

#[derive(Debug, PartialEq, Clone)]
pub(crate) struct AckOrNack {
    records: Vec<Record>,
}

impl AckOrNack {
    /// Extend a packet from a sorted sequence numbers iterator based on mtu.
    /// Notice that a uint24le must be unique in the whole iterator
    pub(crate) fn extend_from<I: Iterator<Item = u32>>(
        mut sorted_seq_nums: I,
        mut mtu: u16,
    ) -> Option<Self> {
        // pack_type(1) + length(2) + single record(4) = 7
        debug_assert!(mtu >= 7, "7 is the least size of mtu");

        let Some(mut first) = sorted_seq_nums.next() else {
            return None;
        };

        let mut records = Vec::new();
        let mut last = first;
        let mut upgrade_flag = true;
        // first byte is pack_type, next 2 bytes are length, the first seq_num takes at least 4
        // bytes
        mtu -= 7;
        loop {
            // we cannot poll sorted_seq_nums because 4 is the least size of a record
            if mtu < 4 {
                break;
            }
            let Some(seq_num) = sorted_seq_nums.next() else {
                break;
            };
            if seq_num == last + 1 {
                if upgrade_flag {
                    mtu -= 3;
                    upgrade_flag = false;
                }
                last = seq_num;
                continue;
            }
            mtu -= 4;
            upgrade_flag = true;
            if first != last {
                records.push(Record::Range(Uint24le(first), Uint24le(last)));
            } else {
                records.push(Record::Single(Uint24le(first)));
            }
            first = seq_num;
            last = seq_num;
        }

        if first != last {
            records.push(Record::Range(Uint24le(first), Uint24le(last)));
        } else {
            records.push(Record::Single(Uint24le(first)));
        }

        Some(Self { records })
    }

    pub(super) fn read(buf: &mut BytesMut) -> Result<Self, CodecError> {
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

    pub(super) fn write(self, buf: &mut BytesMut) {
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
            // pack type
            buf.put_u8(0);
            let mut seq_nums = seq_nums.into_iter();
            let ack = AckOrNack::extend_from(&mut seq_nums, mtu).unwrap();
            ack.write(&mut buf);
            assert_eq!(buf.len(), len);
            assert_eq!(seq_nums.len(), remain);
        }
    }
}
