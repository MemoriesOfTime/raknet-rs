use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use futures::{ready, Stream, StreamExt};
use pin_project_lite::pin_project;

use crate::packet::connected::{self, FrameBody};
use crate::Peer;

pin_project! {
    struct HandShake<F> {
        #[pin]
        frame: F,
    }
}

pub(super) trait HandShaking: Sized {
    fn handshaking(self) -> HandShake<Self>;
}

impl<F> HandShaking for F {
    fn handshaking(self) -> HandShake<Self> {
        HandShake { frame: self }
    }
}

impl<F> Stream for HandShake<F>
where
    F: Stream<Item = (connected::Packet<FrameBody>, Peer)>,
{
    type Item = (connected::Packet<FrameBody>, Peer);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        let Some((packet, peer)) = ready!(this.frame.poll_next_unpin(cx)) else {
            return Poll::Ready(None);
        };
        let connected::Packet::FrameSet(frame_set) = packet else {
            return Poll::Ready(Some((packet, peer)));
        };
        for frame in frame_set.frames {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis();
            match frame.body {
                FrameBody::ConnectedPing { client_timestamp } => {
                    let pong = FrameBody::ConnectedPong {
                        client_timestamp,
                        server_timestamp: timestamp as i64,
                    };
                }
                FrameBody::ConnectionRequest {
                    client_guid,
                    request_timestamp,
                    use_encryption,
                } => {}
                FrameBody::NewIncomingConnection {
                    server_address,
                    system_addresses,
                    request_timestamp,
                    accepted_timestamp,
                } => todo!(),
                _ => todo!(),
            };
        }
        todo!()
    }
}
