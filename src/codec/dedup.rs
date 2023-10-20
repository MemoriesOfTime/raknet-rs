use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use futures::{Sink, Stream};
use pin_project_lite::pin_project;
use tracing::debug;

use crate::codec::PollPacket;
use crate::errors::CodecError;
use crate::packet::connected::Uint24le;
use crate::packet::{connected, PackId, Packet};

#[derive(Debug, Default)]
struct DuplicateWindow {
    /// First unreceived sequence number, start at 0
    first_unreceived: u32,
    // TODO: use bit vec
    /// Record the received status of sequence numbers start at `first_unreceived`
    /// `true` is received and `false` is unreceived
    received_status: VecDeque<bool>,
}

impl DuplicateWindow {
    /// Check whether a sequence number is duplicated
    fn duplicate(&mut self, seq_num: Uint24le) -> bool {
        if seq_num.0 < self.first_unreceived {
            return true;
        }
        let gap = (seq_num.0 - self.first_unreceived) as usize;
        if gap == 0 {
            // received the next sequence number of last received
            // advance the first_unreceived and pop the front of
            // received_status
            self.first_unreceived += 1;
            self.received_status.pop_front();
        } else if gap < self.received_status.len() {
            // received the sequence number that is recorded in received_status
            // check its status to determine whether it is duplicated
            if self.received_status[gap] {
                return true;
            }
            // mark it is received
            self.received_status[gap] = true;
        } else {
            // received the sequence number that exceed received_status, extend
            // the received_status and record the received_status[gap] as received
            let count = gap - self.received_status.len();
            self.received_status
                .extend(std::iter::repeat(false).take(count));
            self.received_status.push_back(true);
        }
        while let Some(true) = self.received_status.front() {
            self.received_status.pop_front();
            self.first_unreceived += 1;
        }
        false
    }
}

pin_project! {
    pub(super) struct Dedup<F> {
        #[pin]
        frame: F,
        windows: HashMap<SocketAddr, DuplicateWindow>
    }
}

pub(super) trait Deduplicated: Sized {
    fn deduplicated(self) -> Dedup<Self>;
}

impl<T> Deduplicated for T
where
    T: Stream<Item = Result<(Packet, SocketAddr), CodecError>>
        + Sink<(Packet, SocketAddr), Error = CodecError>,
{
    fn deduplicated(self) -> Dedup<Self> {
        Dedup {
            frame: self,
            windows: HashMap::new(),
        }
    }
}

impl<F> Stream for Dedup<F>
where
    F: Stream<Item = Result<(Packet, SocketAddr), CodecError>>,
{
    type Item = Result<(Packet, SocketAddr), CodecError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        let (packet, addr) = match this.frame.poll_packet(cx) {
            Ok(v) => v,
            Err(poll) => return poll,
        };

        let Packet::Connected(connected::Packet::FrameSet(mut frame_set)) = packet else {
            return Poll::Ready(Some(Ok((packet, addr))));
        };
        let window = this.windows.entry(addr).or_default();
        frame_set.frames.retain(|frame| {
            let Some(reliable_frame_index) = frame.reliable_frame_index else {
                return true;
            };
            !window.duplicate(reliable_frame_index)
        });
        Poll::Ready(Some(Ok((
            Packet::Connected(connected::Packet::FrameSet(frame_set)),
            addr,
        ))))
    }
}

impl<F> Sink<(Packet, SocketAddr)> for Dedup<F>
where
    F: Sink<(Packet, SocketAddr), Error = CodecError>,
{
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        this.frame.poll_ready(cx)
    }

    fn start_send(
        self: Pin<&mut Self>,
        (packet, addr): (Packet, SocketAddr),
    ) -> Result<(), Self::Error> {
        let this = self.project();
        if let Packet::Connected(connected::Packet::FrameSet(frame_set)) = &packet {
            if matches!(frame_set.inner_pack_id()?, PackId::DisconnectNotification) {
                debug!("disconnect from {}, clean it's dedup window", addr);
                this.windows.remove(&addr);
            }
        };
        this.frame.start_send((packet, addr))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        this.frame.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.poll_flush(cx))?;
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod test {

    use crate::codec::dedup::DuplicateWindow;
    use crate::packet::connected::Uint24le;

    #[test]
    fn test_duplicate_windows_check_ordered() {
        let mut window = DuplicateWindow::default();
        for i in 0..1024 {
            assert!(!window.duplicate(Uint24le(i)));
            assert_eq!(window.first_unreceived, i + 1);
            assert!(window.received_status.len() <= 1);
        }
    }

    #[test]
    fn test_duplicate_windows_check_ordered_dup() {
        let mut window = DuplicateWindow::default();
        for i in 0..512 {
            assert!(!window.duplicate(Uint24le(i)));
            assert_eq!(window.first_unreceived, i + 1);
            assert!(window.received_status.len() <= 1);
        }
        for i in 0..512 {
            assert!(window.duplicate(Uint24le(i)));
        }
    }

    #[test]
    fn test_duplicate_windows_check_gap_dup() {
        let mut window = DuplicateWindow::default();
        assert!(!window.duplicate(Uint24le(0)));
        assert!(!window.duplicate(Uint24le(1)));
        assert!(!window.duplicate(Uint24le(1000)));
        assert!(!window.duplicate(Uint24le(1001)));
        assert!(window.duplicate(Uint24le(1000)));
        assert!(window.duplicate(Uint24le(1001)));
        assert!(!window.duplicate(Uint24le(500)));
        assert!(window.duplicate(Uint24le(500)));
        assert_eq!(window.first_unreceived, 2);
    }

    #[test]
    fn test_duplicate_window_clear_gap_map() {
        let mut window = DuplicateWindow::default();
        for i in (0..256).step_by(2) {
            assert!(!window.duplicate(Uint24le(i)));
        }
        for i in (1..256).step_by(2) {
            assert!(!window.duplicate(Uint24le(i)));
        }
        assert_eq!(window.received_status.len(), 0);
    }
}
