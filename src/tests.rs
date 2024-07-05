use std::collections::HashMap;
use std::iter::repeat;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use log::info;
use minitrace::collector::{SpanId, SpanRecord};
use parking_lot::Mutex;
use tokio::net::UdpSocket;

use crate::client::{self, ConnectTo};
use crate::server::{self, MakeIncoming};

#[allow(clippy::type_complexity)]
pub(crate) struct TestTraceLogGuard {
    spans: Arc<Mutex<Vec<SpanRecord>>>,
    asserts: Box<dyn Fn(&Vec<SpanRecord>)>,
}

impl Drop for TestTraceLogGuard {
    #[allow(clippy::print_stderr)]
    fn drop(&mut self) {
        minitrace::flush();
        // TODO: draw span tree
        let spans = self.spans.lock().clone();
        (self.asserts)(&spans);

        let spans_map: HashMap<SpanId, SpanRecord> = spans
            .iter()
            .map(|span| (span.span_id, span.clone()))
            .collect();
        let adjacency_list: HashMap<SpanId, Vec<SpanId>> = spans.iter().fold(
            std::collections::HashMap::new(),
            |mut map,
             SpanRecord {
                 span_id, parent_id, ..
             }| {
                map.entry(*parent_id).or_default().push(*span_id);
                map
            },
        );
        fn dfs(
            adjacency_list: &HashMap<SpanId, Vec<SpanId>>,
            spans: &HashMap<SpanId, SpanRecord>,
            span_id: SpanId,
            depth: usize,
            begin_at: u64,
        ) {
            let span = &spans[&span_id];
            let mut properties = String::new();
            for (key, value) in &span.properties {
                properties.push_str(&format!(", {}:{}", key, value));
            }
            eprintln!(
                "{}{}(begin:{}ns, duration:{}ns{})",
                "  ".repeat(depth),
                span.name,
                span.begin_time_unix_ns - begin_at,
                span.duration_ns,
                properties
            );
            if let Some(children) = adjacency_list.get(&span_id) {
                for child in children {
                    dfs(adjacency_list, spans, *child, depth + 1, begin_at);
                }
            }
        }

        for root in &adjacency_list[&SpanId::default()] {
            dfs(
                &adjacency_list,
                &spans_map,
                *root,
                0,
                spans_map[root].begin_time_unix_ns,
            );
            eprintln!();
        }
    }
}

#[must_use]
pub(crate) fn test_trace_log_setup() -> TestTraceLogGuard {
    std::env::set_var("RUST_LOG", "trace");
    let (reporter, spans) = minitrace::collector::TestReporter::new();
    minitrace::set_reporter(reporter, minitrace::collector::Config::default());
    env_logger::init();
    TestTraceLogGuard {
        spans,
        asserts: Box::new(|_| {}),
    }
}

#[must_use]
pub(crate) fn test_trace_log_setup_with_assert(
    asserts: impl Fn(&Vec<SpanRecord>) + 'static,
) -> TestTraceLogGuard {
    std::env::set_var("RUST_LOG", "trace");
    let (reporter, spans) = minitrace::collector::TestReporter::new();
    minitrace::set_reporter(reporter, minitrace::collector::Config::default());
    env_logger::init();
    TestTraceLogGuard {
        spans,
        asserts: Box::new(asserts),
    }
}

#[tokio::test(unhandled_panic = "shutdown_runtime")]
async fn test_tokio_udp_works() {
    let _guard = test_trace_log_setup();

    let echo_server = async {
        let config = server::Config::new()
            .send_buf_cap(1024)
            .sever_guid(1919810)
            .advertisement(&b"123456"[..])
            .max_mtu(1500)
            .min_mtu(510)
            .max_pending(1024)
            .support_version(vec![9, 11, 13]);
        let mut incoming = UdpSocket::bind("0.0.0.0:19132")
            .await
            .unwrap()
            .make_incoming(config);
        loop {
            let io = incoming.next().await.unwrap();
            tokio::spawn(async move {
                tokio::pin!(io);
                let mut ticker = tokio::time::interval(Duration::from_millis(10));
                loop {
                    tokio::select! {
                        res = io.next() => {
                            if let Some(data) = res {
                                io.send(data).await.unwrap();
                            } else {
                                info!("client closed connection");
                                break;
                            }
                        }
                        _ = ticker.tick() => {
                            SinkExt::<Bytes>::flush(&mut io).await.unwrap();
                        }
                    };
                }
            });
        }
    };

    tokio::spawn(echo_server);

    let client = async {
        let config = client::Config::new()
            .send_buf_cap(1024)
            .mtu(1000)
            .client_guid(114514)
            .protocol_version(11);
        let mut io = UdpSocket::bind("0.0.0.0:0")
            .await
            .unwrap()
            .connect_to("127.0.0.1:19132", config)
            .await
            .unwrap();
        io.send(Bytes::from_iter(repeat(0xfe).take(256)))
            .await
            .unwrap();
        assert_eq!(
            io.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(256))
        );
        io.send(Bytes::from_iter(repeat(0xfe).take(512)))
            .await
            .unwrap();
        assert_eq!(
            io.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(512))
        );
        io.send(Bytes::from_iter(repeat(0xfe).take(1024)))
            .await
            .unwrap();
        assert_eq!(
            io.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(1024))
        );
        io.send(Bytes::from_iter(repeat(0xfe).take(2048)))
            .await
            .unwrap();
        assert_eq!(
            io.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(2048))
        );
        io.send(Bytes::from_iter(repeat(0xfe).take(4096)))
            .await
            .unwrap();
        assert_eq!(
            io.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(4096))
        );
        SinkExt::<Bytes>::close(&mut io).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    tokio::spawn(client).await.unwrap();
}
