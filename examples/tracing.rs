use std::collections::HashMap;
use std::error::Error;
use std::future::poll_fn;
use std::net::SocketAddr;
use std::process::exit;
use std::time::Duration;

use bytes::Bytes;
use fastrace::collector::{SpanContext, SpanId, SpanRecord, TraceId};
use fastrace::Span;
use futures_lite::StreamExt;
use log::debug;
use raknet_rs::client::{self, ConnectTo};
use raknet_rs::io::{Reader, TraceInfo, Writer};
use raknet_rs::server::{self, MakeIncoming};
use raknet_rs::{Message, Reliability};
use tokio::net::UdpSocket;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let (reporter, spans) = fastrace::collector::TestReporter::new();
    fastrace::set_reporter(
        reporter,
        fastrace::collector::Config::default().report_before_root_finish(true),
    );

    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let local_addr = socket.local_addr()?;
    let mut incoming = socket.make_incoming(
        server::Config::new()
            .send_buf_cap(1024)
            .sever_guid(114514)
            .advertisement(&b"Hello, I am proxy server"[..])
            .min_mtu(500)
            .max_mtu(1400)
            .support_version(vec![9, 11, 13])
            .max_pending(64),
    );

    tokio::spawn(async move {
        loop {
            let (reader, writer) = incoming.next().await.unwrap();
            tokio::spawn(async move {
                tokio::pin!(reader);
                tokio::pin!(writer);
                loop {
                    if let Some(data) = reader.next().await {
                        debug!(
                            "[server] got data: '{}' from {}",
                            String::from_utf8_lossy(&data),
                            reader.get_remote_addr()
                        );
                        let trace_id = reader.last_trace_id().unwrap_or_else(|| {
                            eprintln!("Please run with `--features fastrace/enable` and try again");
                            exit(0)
                        });
                        let root_span = Span::root(
                            "user root span",
                            SpanContext::new(trace_id, SpanId::default()),
                        );
                        // do something with data
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        let _span = Span::enter_with_parent("user child span", &root_span);

                        poll_fn(|cx| writer.as_mut().poll_ready(cx)).await.unwrap();
                        writer
                            .as_mut()
                            .feed(Message::new(Reliability::Reliable, 0, data));
                        poll_fn(|cx| writer.as_mut().poll_flush(cx)).await.unwrap();

                        continue;
                    }
                    break;
                }
            });
        }
    });

    client(local_addr).await?;

    fastrace::flush();
    display(spans.lock().clone());
    Ok(())
}

async fn client(addr: SocketAddr) -> Result<(), Box<dyn Error>> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let (reader, writer) = socket
        .connect_to(
            addr,
            client::Config::new()
                .send_buf_cap(1024)
                .mtu(1000)
                .client_guid(1919810)
                .protocol_version(11),
        )
        .await?;

    tokio::pin!(reader);
    tokio::pin!(writer);

    poll_fn(|cx| writer.as_mut().poll_ready(cx)).await.unwrap();
    writer.as_mut().feed(Message::new(
        Reliability::Reliable,
        0,
        Bytes::from_static(b"User pack1"),
    ));
    // buffered
    writer.as_mut().feed(Message::new(
        Reliability::Reliable,
        0,
        Bytes::from_static(b"User pack1"),
    ));
    poll_fn(|cx| writer.as_mut().poll_flush(cx)).await.unwrap();

    reader.next().await.unwrap();
    reader.next().await.unwrap();
    Ok(())
}

fn display(spans: Vec<SpanRecord>) {
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
        eprintln!("{trace_id:?}",);
        let l = &list[&SpanId::default()];
        for (i, root) in l.iter().enumerate() {
            dfs(&list, &spans_map, *root, 0, i == l.len() - 1);
        }
        eprintln!();
    }
}
