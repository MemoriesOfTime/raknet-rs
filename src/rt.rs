/// Task runtime abstraction
pub trait Runtime<Fut> {
    /// Spawn a task
    fn spawn(&self, fut: Fut);
}
