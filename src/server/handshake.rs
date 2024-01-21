use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures::{ready, Stream, StreamExt};
use pin_project_lite::pin_project;

use crate::packet::connected::FrameBody;

pin_project! {
    pub(super) struct HandShake<F> {
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
    F: Stream<Item = FrameBody>,
{
    type Item = Bytes;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        let Some(body) = ready!(this.frame.poll_next_unpin(cx)) else {
            return Poll::Ready(None);
        };
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
            } => {}
            FrameBody::Disconnect => {
                return Poll::Ready(None);
            }
            _ => todo!(),
        };
        todo!()
    }
}
