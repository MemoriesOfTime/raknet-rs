use bytes::{Buf, BufMut, BytesMut};

use crate::errors::CodecError;
use crate::packet::read_buf;
use crate::utils::{u24, BufExt, BufMutExt};

#[derive(PartialEq, Clone)]
pub(crate) struct AckOrNack {
    pub(crate) records: Vec<Record>,
}

impl std::fmt::Debug for AckOrNack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut records = String::new();
        for record in &self.records {
            match record {
                Record::Range(start, end) => {
                    records += &format!("{}-{},", start, end);
                }
                Record::Single(idx) => {
                    records += &format!("{},", idx);
                }
            }
        }
        if !records.is_empty() {
            records.pop();
        }
        write!(f, "AckOrNack({})", records)
    }
}

impl AckOrNack {
    /// This function implements **Sequence Number Compression** on an `Iterator<u24>`. Consecutive
    /// sequence numbers are grouped into `Record::Range`, while non-consecutive sequence
    /// numbers are stored as `Record::Single`. This approach reduces the data payload size.
    ///
    /// - A `Record::Range` consumes 7 bytes.
    /// - A `Record::Single` consumes 4 bytes.
    pub(crate) fn extend_from<I: Iterator<Item = u24>>(
        mut sorted_seq_nums: I,
        mut mtu: u16,
    ) -> Option<Self> {
        // pack_type(1) + length(2) + single record(4) = 7
        debug_assert!(mtu >= 7, "7 is the least size of mtu");

        let mut first = sorted_seq_nums.next()?;

        let mut records = vec![];
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
                records.push(Record::Range(first, last));
            } else {
                records.push(Record::Single(first));
            }
            first = seq_num;
            last = seq_num;
        }

        if first != last {
            records.push(Record::Range(first, last));
        } else {
            records.push(Record::Single(first));
        }

        Some(Self { records })
    }

    pub(super) fn read(buf: &mut BytesMut) -> Result<Self, CodecError> {
        const MAX_ACKNOWLEDGEMENT_PACKETS: usize = 8192;

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

    pub(crate) fn total_cnt(&self) -> usize {
        self.records.iter().map(|record| record.ack_cnt()).sum()
    }
}

const RECORD_RANGE: u8 = 0;
const RECORD_SINGLE: u8 = 1;

#[derive(PartialEq, Clone)]
pub(crate) enum Record {
    Range(u24, u24),
    Single(u24),
}

impl Record {
    fn read(buf: &mut BytesMut) -> Result<Self, CodecError> {
        let record_type = read_buf!(buf, 1, buf.get_u8());
        match record_type {
            RECORD_RANGE => read_buf!(
                buf,
                6,
                Ok(Record::Range(buf.get_u24_le(), buf.get_u24_le()))
            ),
            RECORD_SINGLE => read_buf!(buf, 3, Ok(Record::Single(buf.get_u24_le()))),
            _ => Err(CodecError::InvalidRecordType(record_type)),
        }
    }

    fn write(self, buf: &mut BytesMut) {
        match self {
            Record::Range(start, end) => {
                buf.put_u8(RECORD_RANGE);
                buf.put_u24_le(start);
                buf.put_u24_le(end);
            }
            Record::Single(idx) => {
                buf.put_u8(RECORD_SINGLE);
                buf.put_u24_le(idx);
            }
        }
    }

    fn ack_cnt(&self) -> usize {
        match self {
            Record::Range(start, end) => usize::from(end - start + 1),
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
            // 3 + 1(4) + 1(4) + 1(4) = 15, no remain
            (vec![1, 1, 1], 15, 0),
            // 3 + 0-999(7) = 10, no remain
            (Vec::from_iter(0..1000), 10, 0),
        ];
        for (seq_nums, len, remain) in test_cases {
            buf.clear();
            // pack type
            buf.put_u8(0);
            let mut seq_nums = seq_nums.into_iter().map(u24::from);
            let ack = AckOrNack::extend_from(&mut seq_nums, mtu).unwrap();
            ack.write(&mut buf);
            assert_eq!(buf.len(), len);
            assert_eq!(seq_nums.len(), remain);
        }
    }
}
