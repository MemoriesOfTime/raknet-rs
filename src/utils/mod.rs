mod bit_queue;
mod log;
mod minitrace;
mod seq_num;

pub(crate) use bit_queue::*;
pub(crate) use log::*;
pub(crate) use minitrace::*;
pub(crate) use seq_num::*;

#[inline]
pub(crate) fn timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
