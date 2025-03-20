pub(crate) struct LegacyCongester {
    state: State,
    cwnd: f32,
    ssthresh: f32,
}

enum State {
    SlowStart,
    CongestionAvoidance,
}

impl LegacyCongester {
    const INITIAL_CWND: f32 = 2.0;

    pub(crate) fn new() -> LegacyCongester {
        LegacyCongester {
            state: State::SlowStart,
            cwnd: Self::INITIAL_CWND,
            ssthresh: f32::INFINITY,
        }
    }
}

impl super::CongestionController for LegacyCongester {
    fn congestion_window(&self) -> usize {
        self.cwnd as usize
    }

    fn on_ack(&mut self, cnt: usize) {
        match self.state {
            State::SlowStart => {
                self.cwnd += cnt as f32;
                if self.cwnd >= self.ssthresh {
                    self.state = State::CongestionAvoidance;
                }
            }
            State::CongestionAvoidance => {
                self.cwnd += cnt as f32 / self.cwnd;
            }
        }
    }

    fn on_nack(&mut self, cnt: usize) {
        // A simple implementation of fast recovery
        // Because we can only receive new ack packets later, which will trigger to quit
        // fast recovery into congestion avoidance.
        self.state = State::CongestionAvoidance;
        self.ssthresh = self.cwnd / 2.0;
        self.cwnd = self.ssthresh + cnt as f32;
    }

    fn on_timeout(&mut self) {
        self.state = State::SlowStart;
        self.ssthresh = self.cwnd / 2.0;
        self.cwnd = Self::INITIAL_CWND;
    }
}
