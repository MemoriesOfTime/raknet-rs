use std::cmp;
use std::time::Duration;

pub(crate) trait Estimator: Send + Sync + 'static {
    /// The current RTO estimation without penalty.
    fn rto(&self) -> Duration;

    /// The current RTT estimation.
    fn rtt(&self) -> Duration;

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
        // Initial RTO value as suggested in RFC6298 2.1 will be 1s.
        // We will use 50ms as the initial value for the latest RTT as well.
        // https://www.rfc-editor.org/rfc/rfc6298.html

        const INITIAL_RTT: Duration = Duration::from_millis(500);
        Self {
            latest: INITIAL_RTT,
            smoothed: None,
            var: Duration::from_secs(0),
        }
    }

    /// The current best RTT estimation.
    fn rtt(&self) -> Duration {
        self.smoothed.unwrap_or(self.latest)
    }

    /// The current RTO estimation.
    pub(crate) fn rto(&self) -> Duration {
        // The granularity of the timer
        // https://www.rfc-editor.org/rfc/rfc9002#section-6.1.2
        // # The RECOMMENDED value of the timer granularity is 1 millisecond.
        const TIMER_GRANULARITY: Duration = Duration::from_millis(1);

        // RFC6298 2.4 suggests a minimum of 1 second, which may be
        // a conservative choice.
        // We disregard the minimum of 1 second for now
        self.rtt() + cmp::max(TIMER_GRANULARITY, 4 * self.var)
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

    fn rtt(&self) -> Duration {
        self.rtt()
    }

    fn update(&mut self, rtt: Duration) {
        self.update(rtt);
    }

    fn clear(&mut self) {
        self.clear();
    }
}
