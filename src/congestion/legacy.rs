pub(crate) struct LegacyCongester;

impl LegacyCongester {
    pub(crate) fn new() -> LegacyCongester {
        LegacyCongester
    }
}

impl super::CongestionController for LegacyCongester {
    fn congestion_window(&self) -> usize {
        // TODO: Implement legacy congestion control
        usize::MAX
    }
}
