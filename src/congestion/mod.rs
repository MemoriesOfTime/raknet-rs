/// Legacy congestion control algorithms
pub(crate) mod legacy;

pub(crate) mod cubic;

pub(crate) mod bbr;

pub(crate) trait CongestionController: Send + Sync + 'static {
    /// Returns the size of the current congestion window in frames
    fn congestion_window(&self) -> usize;
}
