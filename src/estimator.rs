use std::cmp;
use std::time::Duration;

/// The granularity of the timer
const TIMER_GRANULARITY: Duration = Duration::from_millis(1);

pub(crate) trait Estimator {
    /// The current RTO estimation.
    fn rto(&self) -> Duration;

    /// Update the RTT estimator with a new RTT sample.
    fn update(&mut self, rtt: Duration);

    /// Clear the estimator's state.
    fn clear(&mut self);
}

/// RTT estimation based on RFC6298
#[derive(Copy, Clone)]
pub struct RFC6298Impl {
    /// The most recent RTT measurement made when receiving an ack for a previously unacked packet
    latest: Duration,
    /// The smoothed RTT of the connection, computed as described in RFC6298
    smoothed: Option<Duration>,
    /// The RTT variance, computed as described in RFC6298
    var: Duration,
}

impl RFC6298Impl {
    pub(crate) fn new() -> Self {
        Self {
            latest: Duration::from_secs(0),
            smoothed: None,
            var: Duration::from_secs(0),
        }
    }

    /// The current best RTT estimation.
    fn get(&self) -> Duration {
        self.smoothed.unwrap_or(self.latest)
    }

    /// The current RTO estimation.
    pub(crate) fn rto(&self) -> Duration {
        cmp::max(
            self.get() + cmp::max(TIMER_GRANULARITY, 4 * self.var),
            Duration::from_secs(1),
        )
    }

    /// Once smoothed and var are cleared, they should be initialized with the next RTT sample
    pub(crate) fn clear(&mut self) {
        self.smoothed = None;
    }

    pub(crate) fn update(&mut self, rtt: Duration) {
        self.latest = rtt;
        if let Some(smoothed) = self.smoothed {
            let var_sample = if smoothed > rtt {
                smoothed - rtt
            } else {
                rtt - smoothed
            };
            self.var = (3 * self.var + var_sample) / 4;
            self.smoothed = Some((7 * smoothed + rtt) / 8);
        } else {
            self.smoothed = Some(rtt);
            self.var = rtt / 2;
        }
    }
}

impl Estimator for RFC6298Impl {
    fn rto(&self) -> Duration {
        self.rto()
    }

    fn update(&mut self, rtt: Duration) {
        self.update(rtt);
    }

    fn clear(&mut self) {
        self.clear();
    }
}
