mod handler;
mod incoming;

#[cfg(test)]
mod test {
    use bytes::Bytes;
    use futures::{SinkExt, StreamExt};
    use minitrace::collector::{self, ConsoleReporter, SpanContext};
    use minitrace::{Event, Span};
    use tokio::net::UdpSocket;

    use crate::server::handler::offline;
    use crate::server::incoming::{ConfigBuilder, MakeIncoming};
    use crate::utils::{Instrumented, RootSpan};

    #[ignore = "wait until the client is implemented."]
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
        };

        let mut incoming = UdpSocket::bind("0.0.0.0:19132")
            .await
            .unwrap()
            .make_incoming(
                ConfigBuilder::default()
                    .send_buf_cap(1024)
                    .codec(Default::default())
                    .offline(
                        offline::ConfigBuilder::default()
                            .sever_guid(1919810)
                            .advertisement(Bytes::from_static(b"123"))
                            .max_mtu(1500)
                            .min_mtu(510)
                            .max_pending(1024)
                            .support_version(vec![9, 11, 13])
                            .build()
                            .unwrap(),
                    )
                    .build()
                    .unwrap(),
            )
            .enter_on_item::<RootSpan>("incoming");
        loop {
            let mut io = incoming.next().await.unwrap();
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
