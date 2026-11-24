use std::future::Future;
use std::pin::Pin;
use std::sync::LazyLock;
use std::task::{Context, Poll};

use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

/// Network I/O runs on Tokio; the UI awaits results on its own executor.
static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("minits-io")
        .enable_all()
        .build()
        .expect("failed to start the I/O runtime")
});

/// Runs `future` on the I/O runtime. Dropping the returned [`Job`] cancels it.
pub fn spawn<F>(future: F) -> Job<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    Job(RUNTIME.spawn(future))
}

pub struct Job<T>(JoinHandle<T>);

impl<T> Future for Job<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        Pin::new(&mut self.0).poll(cx).map(|result| match result {
            Ok(value) => value,
            Err(error) => std::panic::resume_unwind(error.into_panic()),
        })
    }
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
