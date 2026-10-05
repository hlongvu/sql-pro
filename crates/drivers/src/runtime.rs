//! Bridge between GPUI's executor and Tokio.
//!
//! The network drivers (tokio-postgres, mysql_async) must run on a Tokio
//! runtime. GPUI's async executor is its own; these helpers let any
//! executor await Tokio work by parking the actual future on the shared
//! runtime and awaiting its join handle from the caller's executor.

use std::future::Future;
use std::sync::OnceLock;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

pub fn runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to build tokio runtime")
    })
}

/// Run a future on the shared Tokio runtime and await its result from any
/// executor.
pub async fn on_tokio<T, F>(future: F) -> T
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    runtime()
        .spawn(future)
        .await
        .unwrap_or_else(|e| panic!("tokio task panicked: {e}"))
}

/// Run a closure on a dedicated OS thread, awaiting its result. Unlike
/// [`on_tokio`], this keeps blocking work off the Tokio workers — driver
/// constructors that internally spawn onto the runtime and wait
/// (PostgreSQL's connect) deadlock if run from inside one.
pub async fn on_thread<T, F>(f: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.unwrap_or_else(|_| panic!("driver thread died"))
}

/// Run a blocking closure on the shared runtime's blocking pool.
pub async fn on_blocking<T, F>(f: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    on_tokio(async move { tokio::task::spawn_blocking(f).await.unwrap_or_else(|e| panic!("blocking task panicked: {e}")) }).await
}
