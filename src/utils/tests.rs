use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::task::Waker;

use minitrace::collector::{SpanId, SpanRecord, TraceId};
use parking_lot::Mutex;

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
            eprintln!("[trace_id] {}", trace_id.0);
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
