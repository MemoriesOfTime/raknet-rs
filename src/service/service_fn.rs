use std::fmt::{Debug, Formatter};
use std::future::Future;

#[cfg(test)]
use futures::executor::block_on;

use crate::service::Service;

/// Create a service from an async function
pub const fn service_fn<F>(f: F) -> ServiceFn<F> {
    ServiceFn { f }
}

/// Service hold an async function
#[derive(Clone, Copy)]
pub struct ServiceFn<F> {
    /// inner
    f: F,
}

impl<F> Debug for ServiceFn<F> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "ServiceFn")
    }
}

impl<Req, F, Res, Err, Fut> Service<Req> for ServiceFn<F>
where
    for<'a> F: Fn(Req) -> Fut + 'a,
    for<'a> Fut: Future<Output = Result<Res, Err>> + Send + 'a,
{
    type Error = Err;
    type Response = Res;

    type Future<'a> = impl Future<Output = Result<Res, Err>> + Send + 'a;

    fn call(&self, req: Req) -> Self::Future<'_> {
        (self.f)(req)
    }
}

#[cfg(test)]
#[test]
fn service_fn_should_work() {
    let req = String::from("request");
    let svc_fn = |_: &str| async { Err::<&str, _>("not available") };
    let svc = service_fn(svc_fn);
    let resp = block_on(svc.call(req.as_str()));
    assert_eq!(resp, Err("not available"));
}
