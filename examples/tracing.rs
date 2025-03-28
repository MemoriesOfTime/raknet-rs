#![allow(clippy::print_stdout)]

use std::collections::HashMap;
use std::error::Error;
use std::net::SocketAddr;
use std::process::exit;
use std::time::Duration;

use bytes::Bytes;
use fastrace::collector::{SpanContext, SpanId, SpanRecord, TraceId};
use fastrace::Span;
use futures::{SinkExt, StreamExt};
use raknet_rs::client::{self, ConnectTo};
use raknet_rs::opts::TraceInfo;
use raknet_rs::server::{self, MakeIncoming};
use raknet_rs::Message;
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
            .sever_guid(114514)
            .advertisement("Hello, I am proxy server")
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
                        let trace_id = reader.last_trace_id().unwrap_or_else(|| {
                            println!("Please run with `--features fastrace/enable` and try again");
                            exit(0)
                        });
                        let root_span = Span::root(
                            "user root span",
                            SpanContext::new(trace_id, SpanId::default()),
                        );
                        // do something with data
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        let _span = Span::enter_with_parent("user child span", &root_span);
                        writer.send(Message::new(data)).await.unwrap();
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
    let (src, dst) = socket
        .connect_to(
            addr,
            client::Config::new()
                .mtu(1000)
                .client_guid(1919810)
                .protocol_version(11),
        )
        .await?;
    tokio::pin!(src);
    tokio::pin!(dst);
    dst.send(Message::new(Bytes::from_static(b"User pack1")))
        .await?;
    dst.send(Message::new(Bytes::from_static(b"User pack2")))
        .await?;
    let pack1 = src.next().await.unwrap();
    let pack2 = src.next().await.unwrap();
    assert_eq!(pack1, Bytes::from_static(b"User pack1"));
    assert_eq!(pack2, Bytes::from_static(b"User pack2"));
    Ok(())
}

fn display(spans: Vec<SpanRecord>) {
    let spans_map: HashMap<SpanId, SpanRecord> = spans
        .iter()
        .map(|span| (span.span_id, span.clone()))
        .collect();
    let adjacency_lists: HashMap<TraceId, HashMap<SpanId, Vec<SpanId>>> = spans.iter().fold(
        HashMap::new(),
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
        println!(
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
        println!("trace_id: {}", trace_id.0);
        let l = &list[&SpanId::default()];
        for (i, root) in l.iter().enumerate() {
            dfs(&list, &spans_map, *root, 0, i == l.len() - 1);
        }
        println!();
    }
}
