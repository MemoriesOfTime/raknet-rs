use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::{ready, Stream, StreamExt};
use pin_project_lite::pin_project;

use crate::packet::connected::{self, AckOrNack, FrameBody, FrameSet};

pin_project! {
    pub(super) struct AckHandler<F> {
        #[pin]
        frame: F,
        resending: HashMap<u32, FrameSet<FrameBody>>
    }
}

impl<F> AckHandler<F> {
    fn on_ack(&mut self, ack: AckOrNack) {
        for record in ack.records {
            let (start, end) = match record {
                connected::Record::Range(start, end) => (start.0, end.0),
                connected::Record::Single(single) => (single.0, single.0),
            };
            for seq_num in start..=end {
                self.resending.remove(&seq_num);
            }
        }
    }
}

pub(super) trait Acknowledge: Sized {
    fn ack(self) -> AckHandler<Self>;
}

impl<F> Acknowledge for F {
    fn ack(self) -> AckHandler<Self> {
        AckHandler {
            frame: self,
            resending: HashMap::new(),
        }
    }
}

impl<F> Stream for AckHandler<F>
where
    F: Stream<Item = connected::Packet<FrameBody>>,
{
    type Item = Bytes;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        let Some(pack) = ready!(this.frame.poll_next_unpin(cx)) else {
            return Poll::Ready(None);
        };
        todo!()
    }
}

struct SlidingWindow {
    mtu: u16,
    cwnd: f32,
    ss_thresh: f32,
    estimate_rtt: f32,
    last_rtt: f32,
    deviation_rtt: f32,
    oldest_unsent_ack: u64,
}

impl SlidingWindow {
    fn new(mtu: u16) -> Self {
        Self {
            mtu,
            cwnd: f32::from(mtu),
            ss_thresh: 0.0,
            estimate_rtt: -1.0,
            last_rtt: -1.0,
            deviation_rtt: -1.0,
            oldest_unsent_ack: 0,
        }
    }
}
