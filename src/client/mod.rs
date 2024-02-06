mod conn;
mod handler;

#[cfg(test)]
mod test {
    use bytes::Bytes;
    use futures::sink::SinkExt;
    use minitrace::collector::{self, ConsoleReporter, SpanContext};
    use minitrace::{Event, Span};
    use tokio::net::UdpSocket;

    use crate::client::conn::{ConfigBuilder, ConnectTo, IO};
    use crate::client::handler::offline;

    #[ignore = "wait until the client is implemented."]
    #[tokio::test]
    async fn test_tokio_conn_works() {
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
        };

        let mut io: IO = UdpSocket::bind("0.0.0.0:0").await.unwrap().connect_to(
            "127.0.0.1:19132".parse().unwrap(),
            ConfigBuilder::default()
                .send_buf_cap(1024)
                .codec(Default::default())
                .offline(
                    offline::ConfigBuilder::default()
                        .mtu(1000)
                        .client_guid(114514)
                        .protocol_version(11)
                        .build()
                        .unwrap(),
                )
                .build()
                .unwrap(),
        );

        io.send(Bytes::from_static(b"1919810")).await.unwrap();
    }
}
