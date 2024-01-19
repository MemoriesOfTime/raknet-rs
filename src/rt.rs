use futures::Future;

/// Multithread task runtime abstraction
///
/// Example implementation:
///
/// ```no_run
/// struct Tokio;
/// impl Runtime for Tokio {
///     fn spawn<T>(&self, fut: T)
///     where
///         T: Future + Send + 'static,
///         T::Output + Send + 'static,
///     {
///         tokio::spawn(fut);
///     }
/// }
/// ```
pub trait Runtime {
    /// Spawn a task
    fn spawn<T>(&self, fut: T)
    where
        T: Future + Send + 'static,
        T::Output: Send + 'static;
}
