#![allow(clippy::use_debug)]

use std::collections::HashMap;
use std::iter::repeat;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use log::info;
use minitrace::collector::{SpanId, SpanRecord, TraceId};
use parking_lot::Mutex;
use tokio::net::UdpSocket;

use crate::client::{self, ConnectTo};
use crate::io::{Ping, TraceInfo, IO};
use crate::server::{self, MakeIncoming};
use crate::{Message, Reliability};

#[allow(clippy::type_complexity)]
pub(crate) struct TestTraceLogGuard {
    spans: Arc<Mutex<Vec<SpanRecord>>>,
}

impl Drop for TestTraceLogGuard {
    #[allow(clippy::print_stderr)]
    fn drop(&mut self) {
        minitrace::flush();

        let spans = self.spans.lock().clone();
        let spans_map: HashMap<SpanId, SpanRecord> = spans
            .iter()
            .map(|span| (span.span_id, span.clone()))
            .collect();
        let adjacency_lists: HashMap<TraceId, HashMap<SpanId, Vec<SpanId>>> = spans.iter().fold(
            std::collections::HashMap::new(),
            |mut map,
             SpanRecord {
                 trace_id,
                 span_id,
                 parent_id,
                 ..
             }| {
                map.entry(*trace_id)
                    .or_default()
                    .entry(*parent_id)
                    .or_default()
                    .push(*span_id);
                map
            },
        );
        fn dfs(
            adjacency_list: &HashMap<SpanId, Vec<SpanId>>,
            spans: &HashMap<SpanId, SpanRecord>,
            span_id: SpanId,
            depth: usize,
            last: bool,
        ) {
            let span = &spans[&span_id];
            let mut properties = String::new();
            for (key, value) in &span.properties {
                properties.push_str(&format!("{}: {}, ", key, value));
            }
            let mut events = String::new();
            for ev in &span.events {
                events.push_str(&format!("'{}'", ev.name));
            }
            let prefix = if depth == 0 {
                String::new()
            } else if last {
                "╰".to_owned() + &"─".repeat(depth) + " "
            } else {
                "├".to_owned() + &"─".repeat(depth) + " "
            };
            eprintln!(
                "{}{}({}{{{}}}) [{}us]",
                prefix,
                span.name,
                properties,
                events,
                span.duration_ns as f64 / 1_000.0,
            );
            if let Some(children) = adjacency_list.get(&span_id) {
                for (i, child) in children.iter().enumerate() {
                    dfs(
                        adjacency_list,
                        spans,
                        *child,
                        depth + 1,
                        i == children.len() - 1 && last,
                    );
                }
            }
        }
        for (trace_id, list) in adjacency_lists {
            if list.is_empty() {
                continue;
            }
            eprintln!("{trace_id:?}");
            let l = &list[&SpanId::default()];
            for (i, root) in l.iter().enumerate() {
                dfs(&list, &spans_map, *root, 0, i == l.len() - 1);
            }
            eprintln!();
        }
    }
}

#[must_use = "guard should be kept alive to keep the trace log"]
pub(crate) fn test_trace_log_setup() -> TestTraceLogGuard {
    std::env::set_var("RUST_LOG", "trace");
    let (reporter, spans) = minitrace::collector::TestReporter::new();
    minitrace::set_reporter(
        reporter,
        minitrace::collector::Config::default().report_before_root_finish(true),
    );
    let _ignore = env_logger::try_init();
    TestTraceLogGuard { spans }
}

fn make_server_conf() -> server::Config {
    server::Config::new()
        .send_buf_cap(1024)
        .sever_guid(1919810)
        .advertisement(&b"123456"[..])
        .max_mtu(1500)
        .min_mtu(510)
        .max_pending(1024)
        .support_version(vec![9, 11, 13])
}

fn make_client_conf() -> client::Config {
    client::Config::new()
        .send_buf_cap(1024)
        .mtu(1000)
        .client_guid(114514)
        .protocol_version(11)
}

#[tokio::test(unhandled_panic = "shutdown_runtime")]
async fn test_tokio_udp_works() {
    let _guard = test_trace_log_setup();

    let echo_server = async {
        let mut incoming = UdpSocket::bind("0.0.0.0:19132")
            .await
            .unwrap()
            .make_incoming(make_server_conf());
        loop {
            let io = incoming.next().await.unwrap();
            tokio::spawn(async move {
                tokio::pin!(io);
                let mut ticker = tokio::time::interval(Duration::from_millis(20));
                loop {
                    tokio::select! {
                        Some(data) = io.next() => {
                            io.feed(data).await.unwrap();
                            info!("last trace id: {:?}", (*io).last_trace_id());
                        }
                        _ = ticker.tick() => {
                            io.flush().await.unwrap();
                        }
                    };
                }
            });
        }
    };

    tokio::spawn(echo_server);

    let client = async {
        let io = UdpSocket::bind("0.0.0.0:0")
            .await
            .unwrap()
            .connect_to("127.0.0.1:19132", make_client_conf())
            .await
            .unwrap();
        tokio::pin!(io);
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
        io.as_mut().ping().await.unwrap();
        io.send(Bytes::from_iter(repeat(0xfe).take(2048)))
            .await
            .unwrap();
        io.as_mut().ping().await.unwrap();
        assert_eq!(
            io.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(2048))
        );
        io.as_mut().ping().await.unwrap();
        io.send(Bytes::from_iter(repeat(0xfe).take(4096)))
            .await
            .unwrap();
        assert_eq!(
            io.next().await.unwrap(),
            Bytes::from_iter(repeat(0xfe).take(4096))
        );
    };

    tokio::spawn(client).await.unwrap();
}

#[tokio::test(unhandled_panic = "shutdown_runtime")]
async fn test_4way_handshake_client_close() {
    let _guard = test_trace_log_setup();

    let server = async {
        let mut incoming = UdpSocket::bind("0.0.0.0:19133")
            .await
            .unwrap()
            .make_incoming(make_server_conf());
        loop {
            let io = incoming.next().await.unwrap();
            tokio::spawn(async move {
                tokio::pin!(io);
                let mut ticker = tokio::time::interval(Duration::from_millis(5));
                loop {
                    tokio::select! {
                        res = io.next() => {
                            if let Some(res) = res {
                                io.feed(res).await.unwrap();
                            } else {
                                break;
                            }
                        }
                        _ = ticker.tick() => {
                            // flush periodically to ensure all missing packets/ack are sent
                            io.flush().await.unwrap();
                        }
                    };
                }
                info!("connection closed by client, close the io");
                io.close().await.unwrap();
                info!("io closed");
            });
        }
    };

    let client = async {
        let io = UdpSocket::bind("0.0.0.0:0")
            .await
            .unwrap()
            .connect_to("127.0.0.1:19133", make_client_conf())
            .await
            .unwrap();

        let (src, dst) = IO::split(io);

        tokio::pin!(src);
        tokio::pin!(dst);

        let huge_msg = Bytes::from_iter(repeat(0xfe).take(2048));
        dst.send(Message::new(
            Reliability::ReliableOrdered,
            0,
            huge_msg.clone(),
        ))
        .await
        .unwrap();
        assert_eq!(src.next().await.unwrap(), huge_msg);

        dst.close().await.unwrap();

        info!("client closed the connection, wait for server to close");

        let mut ticker = tokio::time::interval(Duration::from_millis(10));
        let mut last_2msl = false;
        let last_timer = tokio::time::sleep(Duration::from_millis(200));
        tokio::pin!(last_timer);
        loop {
            tokio::select! {
                None = src.next(), if !last_2msl => {
                    info!("received close notification from server, wait for 200ms(2MSL for test purpose)");
                    last_2msl = true;
                }
                _ = ticker.tick() => {
                    // flush periodically to ensure all missing packets/ack are sent
                    dst.flush().await.unwrap();
                }
                _ = &mut last_timer, if last_2msl => {
                    break;
                }
            };
        }
    };

    tokio::spawn(server);
    tokio::spawn(client).await.unwrap();
}
