use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::task::Waker;

use bytes::Bytes;
use futures::{Sink, Stream, StreamExt};

mod flusher;
mod sim_net;
mod tracing;

pub(crate) use flusher::*;
pub(crate) use sim_net::*;
pub(crate) use tracing::*;

use crate::Message;

pub(crate) struct TestWaker {
    pub(crate) woken: AtomicBool,
}

impl std::task::Wake for TestWaker {
    fn wake_by_ref(self: &Arc<Self>) {
        self.woken.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
}

impl TestWaker {
    pub(crate) fn create() -> Waker {
        Arc::new(TestWaker {
            woken: AtomicBool::new(false),
        })
        .into()
    }

    pub(crate) fn pair() -> (Waker, Arc<Self>) {
        let arc = Arc::new(TestWaker {
            woken: AtomicBool::new(false),
        });
        (Waker::from(Arc::clone(&arc)), arc)
    }
}

pub(crate) fn spawn_echo_server(
    incoming: impl Stream<
            Item = (
                impl Stream<Item = Bytes> + Send + Sync + 'static,
                impl Sink<Message, Error = io::Error> + Send + Sync + 'static,
            ),
        > + Send
        + Sync
        + 'static,
) {
    tokio::spawn(async move {
        tokio::pin!(incoming);
        loop {
            let (reader, sender) = incoming.next().await.unwrap();
            tokio::spawn(async move {
                tokio::pin!(reader);
                let (mut flusher, _notify, tx) = Flusher::new(Box::pin(sender));
                tokio::spawn(async move {
                    flusher.run().await.unwrap();
                });
                while let Some(data) = reader.next().await {
                    tx.send(Message::new(data)).await.unwrap();
                }
            });
        }
    });
}
