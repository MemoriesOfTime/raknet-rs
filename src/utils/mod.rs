mod bit_queue;
mod fastrace;
mod hash;
mod log;
mod reactor;
mod seq_num;

pub(crate) use self::bit_queue::*;
pub(crate) use self::fastrace::*;
pub(crate) use self::hash::*;
pub(crate) use self::log::*;
pub(crate) use self::reactor::*;
pub(crate) use self::seq_num::*;

/// Test utils.
#[cfg(test)]
pub(crate) mod tests;

#[inline]
pub(crate) fn timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
