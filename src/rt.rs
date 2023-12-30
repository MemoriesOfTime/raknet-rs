use futures::Future;

/// Task runtime abstraction
pub trait Runtime<T: Future> {
    type Output;

    /// Spawn a task
    fn spawn(fut: T) -> Self::Output;
}

#[derive(Debug, Clone, Copy)]
pub struct BlockOn;

impl<T: Future> Runtime<T> for BlockOn {
    type Output = T::Output;

    fn spawn(fut: T) -> Self::Output {
        futures::executor::block_on(fut)
    }
}

#[cfg(feature = "rt-tokio")]
#[derive(Debug, Clone, Copy)]
pub struct Tokio;

#[cfg(feature = "rt-tokio")]
impl<T> Runtime<T> for Tokio
where
    T: Future + Send + 'static,
    T::Output: Send + 'static,
{
    type Output = tokio::task::JoinHandle<T::Output>;

    fn spawn(fut: T) -> Self::Output {
        tokio::spawn(fut)
    }
}
