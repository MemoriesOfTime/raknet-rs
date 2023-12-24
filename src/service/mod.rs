/// Utils function for service building
mod service_fn;

pub use service_fn::service_fn;

/// Tower's `Service` is a highly valuable tool for application development. Nevertheless, it
/// appears somewhat intricate when applied to Raknet services. Here, I've made a streamlined and
/// swifter alternative to the tower's `Service`, which enable the `async_fn_in_trait` feature for
/// static dispatching `Service::call`.
///
/// Context can be omitted within a `Service` or provided by a global variable with a static
/// lifetime. This capability allows for greater flexibility.
pub trait Service<Request> {
    /// Error made by this service
    type Error;

    /// Response made by this service
    type Response;

    /// Process the request asynchronously
    #[allow(async_fn_in_trait)] // No need to consider the auto trait for now.
    async fn call(&self, req: Request) -> Result<Self::Response, Self::Error>
    where
        Self: Sized;
}

#[cfg(test)]
mod test {
    use futures::executor::block_on;

    use super::*;

    struct EchoService;

    impl Service<String> for EchoService {
        type Error = String;
        type Response = String;

        async fn call(&self, req: String) -> Result<Self::Response, Self::Error> {
            Ok(req)
        }
    }

    struct DummyMiddleware<S> {
        good_smell: u64,
        inner: S,
    }

    impl<S> Service<String> for DummyMiddleware<S>
    where
        S: Service<String, Response = String, Error = String>,
    {
        type Error = S::Error;
        type Response = S::Response;

        async fn call(&self, req: String) -> Result<Self::Response, Self::Error> {
            self.inner.call(format!("{}-{req}", self.good_smell)).await
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
