use futures::Stream;

use super::handler::offline;
use super::IO;
use crate::codec;

/// Incoming implementation by using tokio's UDP framework
mod tokio;

/// Incoming config
#[derive(Debug, Clone, derive_builder::Builder)]
pub struct Config {
    /// The send buffer of each IO polled by the incoming
    send_buf_cap: usize,
    codec: codec::Config,
    offline: offline::Config,
}

pub trait MakeIncoming: Sized {
    fn make_incoming(self, config: Config) -> impl Stream<Item = IO>;
}

#[cfg(test)]
mod test {

    use bytes::Bytes;
    use futures::{SinkExt, StreamExt};
    use minitrace::collector::{self, ConsoleReporter, SpanContext};
    use minitrace::{Event, Span};
    use tokio::net::UdpSocket;

    use crate::server::handler::offline;
    use crate::server::incoming::{Config, MakeIncoming};
    use crate::server::IO;

    #[tokio::test]
    async fn test_tokio_incoming_works() {
        minitrace::set_reporter(ConsoleReporter, collector::Config::default());

        env_logger::Builder::from_default_env()
            .format(|_, record| {
                // Add a event to the current local span representing the log record
                Event::add_to_local_parent(record.level().as_str(), || {
                    [("message".into(), record.args().to_string().into())]
                });
                // write nothing as we already use `ConsoleReporter`
                Ok(())
            })
            .filter_level(log::LevelFilter::Trace)
            .init();

        {
            let root = Span::root("test_tokio_incoming_works", SpanContext::random());
            Event::add_to_parent("it works!", &root, || []);
        }

        let mut incoming = UdpSocket::bind("0.0.0.0:19132")
            .await
            .unwrap()
            .make_incoming(Config {
                send_buf_cap: 1024,
                codec: Default::default(),
                offline: offline::Config {
                    sever_guid: 0,
                    advertisement: Bytes::from_static(b"MCPE"),
                    min_mtu: 500,
                    max_mtu: 1300,
                    support_version: vec![9, 11, 13],
                    max_pending: 512,
                },
            });
        loop {
            let mut io: IO = incoming.next().await.unwrap();
            tokio::spawn(async move {
                loop {
                    let msg: Bytes = io.next().await.unwrap();
                    log::info!("msg: {}", String::from_utf8_lossy(&msg));
                    io.send(msg).await.unwrap();
                    io.send(Bytes::from_static(b"114514")).await.unwrap();
                }
            });
        }
    }
}
