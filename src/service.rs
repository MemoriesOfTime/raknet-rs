/// Utils function for service building
mod service_fn;

use std::future::Future;

pub use service_fn::service_fn;

/// Tower's `Service` is a highly valuable tool for application development. Nevertheless, it
/// appears somewhat intricate when applied to Raknet services. Here, I've made a streamlined and
/// swifter alternative to the tower's `Service`, which is inspired by
/// [motore](https://github.com/cloudwego/motore)
///
/// Context can be omitted within a `Service` or provided by a global variable with a static
/// lifetime. This capability allows for greater flexibility. Furthermore, The
/// Generic Associated Types (GAT) enables `Future` to carry a lifetime marker, effectively
/// mitigating the overhead associated with `Box::pin` when implementing a Service.
pub trait Service<Request> {
    /// Error made by this service
    type Error;

    /// Response made by this service
    type Response;

    /// Future of this service
    type Future<'a>: Future<Output = Result<Self::Response, Self::Error>> + Send + 'a
        where
            Self: 'a;

    /// Process the request asynchronously
    fn call(&self, req: Request) -> Self::Future<'_>;
}

#[cfg(test)]
mod test {
    use futures::executor::block_on;

    use super::*;

    struct EchoService;

    impl Service<String> for EchoService {
        type Error = String;
        type Response = String;

        type Future<'a> = impl Future<Output = Result<Self::Response, Self::Error>> + Send + 'a;

        fn call(&self, req: String) -> Self::Future<'_> {
            async move { Ok(req) }
        }
    }

    struct DummyMiddleware<S> {
        good_smell: u64,
        inner: S,
    }

    impl<S> Service<String> for DummyMiddleware<S>
        where
            S: Service<String, Response = String, Error = String> + Sync + Send + 'static,
    {
        type Error = S::Error;
        type Response = S::Response;

        type Future<'a> = impl Future<Output = Result<S::Response, S::Error>> + Send + 'a;

        fn call(&self, req: String) -> Self::Future<'_> {
            async move { self.inner.call(format!("{}-{req}", self.good_smell)).await }
        }
    }

    #[test]
    fn echo_service_test() {
        let echo = EchoService;
        let fut = echo.call("request".to_string());
        let resp = block_on(fut);
        assert_eq!(resp, Ok("request".to_string()));
    }

    #[test]
    fn dummy_middleware_test() {
        let svc = DummyMiddleware {
            good_smell: 114514,
            inner: EchoService,
        };
        let fut = svc.call("request".to_string());
        let resp = block_on(fut);
        assert_eq!(resp, Ok("114514-request".to_string()));
    }
}
