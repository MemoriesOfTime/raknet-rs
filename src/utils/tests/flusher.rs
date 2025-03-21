use std::io;
use std::sync::Arc;
use std::time::Duration;

use async_channel::{Receiver, Sender};
use futures::{Sink, SinkExt};
use tokio::sync::Notify;
use tokio::time;

use crate::Message;

pub(crate) struct Flusher<W> {
    writer: W,
    ticker: time::Interval,
    notifier: Arc<Notify>,
    rx: Receiver<Message>,
}

impl<W: Sink<Message, Error = io::Error> + Unpin + Send + Sync + 'static> Flusher<W> {
    const DEFAULT_FLUSH_DELAY: Duration = Duration::from_millis(1);

    pub(crate) fn new(writer: W) -> (Self, Arc<Notify>, Sender<Message>) {
        let mut ticker = time::interval(Self::DEFAULT_FLUSH_DELAY);
        ticker.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
        let notifier = Arc::new(Notify::new());
        let (tx, rx) = async_channel::unbounded();
        let me = Self {
            writer,
            ticker,
            notifier: notifier.clone(),
            rx,
        };
        (me, notifier, tx)
    }

    pub(crate) async fn run(&mut self) -> io::Result<()> {
        loop {
            tokio::select! {
                _ = self.ticker.tick() => {
                    self.writer.flush().await?;
                }
                _ = self.notifier.notified() => {
                    self.writer.flush().await?;
                }
                Ok(msg) = self.rx.recv() => {
                    self.writer.feed(msg).await?;
                }
            }
        }
    }
}
