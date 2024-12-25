use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::task::Waker;

mod sim_net;
mod tracing;

pub(crate) use sim_net::*;
pub(crate) use tracing::*;

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
