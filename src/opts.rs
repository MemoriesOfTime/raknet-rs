use std::future::Future;
use std::io;
use std::pin::Pin;

use fastrace::collector::TraceId;
use futures::{Sink, SinkExt};

use crate::packet::connected::FrameBody;
use crate::utils::timestamp;

/// Trace info extension for server
pub trait TraceInfo {
    fn last_trace_id(&self) -> Option<TraceId>;
}

/// Ping extension for client, experimental
pub trait Ping {
    fn ping(self: Pin<&mut Self>) -> impl Future<Output = Result<(), io::Error>> + Send;
}

impl<S> Ping for S
where
    S: Sink<FrameBody, Error = io::Error> + Send,
{
    async fn ping(mut self: Pin<&mut Self>) -> Result<(), io::Error> {
        self.send(FrameBody::ConnectedPing {
            client_timestamp: timestamp(),
        })
        .await
    }
}
