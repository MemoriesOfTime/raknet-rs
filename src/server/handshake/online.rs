use std::alloc::System;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use bytes::{Buf, Bytes};
use futures::{ready, Stream, StreamExt};
use pin_project_lite::pin_project;

use super::offline::Peer;
use crate::packet::connected::FrameBody;
use crate::packet::{connected, PackType};

pin_project! {
    struct OnlineHandShake<F> {
        #[pin]
        frame: F,
    }
}

impl<F> Stream for OnlineHandShake<F>
where
    F: Stream<Item = (connected::Packet<Bytes>, Peer)>,
{
    type Item = (connected::Packet<Bytes>, Peer);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        let Some((packet, peer)) = ready!(this.frame.poll_next_unpin(cx)) else {
            return Poll::Ready(None);
        };
        let connected::Packet::FrameSet(frame_set) = packet else {
            return Poll::Ready(Some((packet, peer)));
        };
        for frame in frame_set.frames {
            let body = FrameBody::read(frame.body).expect("TODO");
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis();
            match body {
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
