//# https://www.rfc-editor.org/rfc/rfc3168#section-5
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Eq, Ord, Hash)]
pub enum ExplicitCongestionNotification {
    /// The not-ECT codepoint '00' indicates a packet that is not using ECN.
    NotEct = 0b00,

    /// ECT(1) is set by the data sender to indicate that the end-points of the transport
    /// protocol are ECN-capable.
    Ect1 = 0b01,

    /// ECT(0) is set by the data sender to indicate that the end-points of the transport
    /// protocol are ECN-capable.
    /// Protocols and senders that only require a single ECT codepoint SHOULD use ECT(0).
    Ect0 = 0b10,

    /// The CE codepoint '11' is set by a router to indicate congestion to the end nodes.
    Ce = 0b11,
}

impl Default for ExplicitCongestionNotification {
    #[inline]
    fn default() -> Self {
        Self::NotEct
    }
}

impl ExplicitCongestionNotification {
    /// Create a `ExplicitCongestionNotification` from the ECN field in the IP header
    #[inline]
    pub fn new(ecn_field: u8) -> Self {
        match ecn_field & 0b11 {
            0b00 => ExplicitCongestionNotification::NotEct,
            0b01 => ExplicitCongestionNotification::Ect1,
            0b10 => ExplicitCongestionNotification::Ect0,
            0b11 => ExplicitCongestionNotification::Ce,
            _ => unreachable!(),
        }
    }

    /// Returns true if congestion was experienced by the peer
    #[inline]
    pub fn congestion_experienced(self) -> bool {
        self == Self::Ce
    }

    /// Returns true if ECN is in use
    #[inline]
    pub fn using_ecn(self) -> bool {
        self != Self::NotEct
    }
}
