use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use futures::{Sink, Stream};
use pin_project_lite::pin_project;
use tracing::debug;

use crate::errors::CodecError;
use crate::packet::connected::Uint24le;
use crate::packet::{connected, PackId, Packet};

#[derive(Debug, Default)]
struct DuplicateWindow {
    read_index: u32,
    // TODO: use bit vec
    gap_map: VecDeque<bool>,
}

impl DuplicateWindow {
    fn new() -> Self {
        Self {
            read_index: 0,
            gap_map: VecDeque::new(),
        }
    }

    fn duplicate(&mut self, seq_num: Uint24le) -> bool {
        if seq_num.0 < self.read_index {
            return true;
        }
        let gap = (seq_num.0 - self.read_index) as usize;
        if gap == 0 {
            self.read_index += 1;
            self.gap_map.pop_front();
        } else if gap < self.gap_map.len() {
            if self.gap_map[gap] {
                self.gap_map[gap] = false;
            } else {
                return true;
            }
        } else {
            let count = gap - self.gap_map.len();
            self.gap_map.extend(std::iter::repeat(true).take(count));
            self.gap_map.push_back(false);
        }
        while let Some(false) = self.gap_map.front() {
            self.gap_map.pop_front();
            self.read_index += 1;
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

        let Some(res) = ready!(this.frame.poll_next(cx)) else {
            return Poll::Ready(None);
        };
        let (packet, addr) = match res {
            Ok((packet, addr)) => (packet, addr),
            Err(err) => {
                return Poll::Ready(Some(Err(err)));
            }
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
        let mut window = DuplicateWindow::new();
        for i in 0..1024 {
            assert!(!window.duplicate(Uint24le(i)));
            assert_eq!(window.read_index, i + 1);
            assert!(window.gap_map.len() <= 1);
        }
    }

    #[test]
    fn test_duplicate_windows_check_ordered_dup() {
        let mut window = DuplicateWindow::new();
        for i in 0..512 {
            assert!(!window.duplicate(Uint24le(i)));
            assert_eq!(window.read_index, i + 1);
            assert!(window.gap_map.len() <= 1);
        }
        for i in 0..512 {
            assert!(window.duplicate(Uint24le(i)));
        }
    }

    #[test]
    fn test_duplicate_windows_check_gap_dup() {
        let mut window = DuplicateWindow::new();
        assert!(!window.duplicate(Uint24le(0)));
        assert!(!window.duplicate(Uint24le(1)));
        assert!(!window.duplicate(Uint24le(1000)));
        assert!(!window.duplicate(Uint24le(1001)));
        assert!(window.duplicate(Uint24le(1000)));
        assert!(window.duplicate(Uint24le(1001)));
        assert!(!window.duplicate(Uint24le(500)));
        assert!(window.duplicate(Uint24le(500)));
        assert_eq!(window.read_index, 2);
    }

    #[test]
    fn test_duplicate_window_clear_gap_map() {
        let mut window = DuplicateWindow::new();
        for i in (0..256).step_by(2) {
            assert!(!window.duplicate(Uint24le(i)));
        }
        for i in (1..256).step_by(2) {
            assert!(!window.duplicate(Uint24le(i)));
        }
        assert_eq!(window.gap_map.len(), 0);
    }
}
