use std::cmp;
use std::time::Duration;

/// The granularity of the timer
const TIMER_GRANULARITY: Duration = Duration::from_millis(1);
/// Maximum ACK delay (i.e from the time the frame set is received to the time the ack is sent).
const MAX_ACK_DELAY: Duration = Duration::from_millis(25);

pub(crate) trait RttEstimator {
    /// The current best RTT estimation.
    fn get(&self) -> Duration;

    /// Conservative estimate of RTT (i.e the maximum RTT).
    fn conservative(&self) -> Duration;

    /// Minimum RTT registered so far for this estimator.
    fn min(&self) -> Duration;

    /// Probe timeout(exclude ack delay).
    fn pto_base(&self) -> Duration;

    /// Update the RTT estimator with a new RTT sample.
    fn update(&mut self, guessed_ack_delay: Duration, rtt: Duration);
}

pub(crate) trait AckDelayEstimator {
    /// Get the current best ack delay estimation.
    fn get(&self) -> Duration;

    /// Update the estimator with inflight packets count.
    fn update(&mut self, inflight: u64);
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
    /// The minimum RTT seen in the connection, ignoring ack delay.
    min: Duration,
}

impl RFC6298Impl {
    fn new(initial_rtt: Duration) -> Self {
        Self {
            latest: initial_rtt,
            smoothed: None,
            var: initial_rtt / 2,
            min: initial_rtt,
        }
    }

    /// The current best RTT estimation.
    pub fn get(&self) -> Duration {
        self.smoothed.unwrap_or(self.latest)
    }

    /// Conservative estimate of RTT
    ///
    /// Takes the maximum of smoothed and latest RTT, as recommended
    /// in 6.1.2 of the recovery spec (draft 29).
    pub fn conservative(&self) -> Duration {
        self.get().max(self.latest)
    }

    /// Minimum RTT registered so far for this estimator.
    pub fn min(&self) -> Duration {
        self.min
    }

    // PTO computed as described in RFC9002#6.2.1
    pub(crate) fn pto_base(&self) -> Duration {
        self.get() + cmp::max(4 * self.var, TIMER_GRANULARITY)
    }

    pub(crate) fn update(&mut self, guessed_ack_delay: Duration, rtt: Duration) {
        self.latest = rtt;
        // min_rtt ignores ack delay.
        self.min = cmp::min(self.min, self.latest);
        // Based on RFC6298.
        if let Some(smoothed) = self.smoothed {
            let adjusted_rtt = if self.min + guessed_ack_delay <= self.latest {
                self.latest - guessed_ack_delay
            } else {
                self.latest
            };
            let var_sample = if smoothed > adjusted_rtt {
                smoothed - adjusted_rtt
            } else {
                adjusted_rtt - smoothed
            };
            self.var = (3 * self.var + var_sample) / 4;
            self.smoothed = Some((7 * smoothed + adjusted_rtt) / 8);
        } else {
            self.smoothed = Some(self.latest);
            self.var = self.latest / 2;
            self.min = self.latest;
        }
    }
}
