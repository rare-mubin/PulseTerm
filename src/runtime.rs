use std::sync::OnceLock;

use futures::channel::oneshot;
use tokio::runtime::Runtime;

/// russh is written against tokio; gpui's executor is smol-based. Rather than
/// pull in a compat shim, the whole SSH layer lives on its own multi-threaded
/// tokio runtime and every call from the UI crosses over via a oneshot.
///
/// The runtime is process-wide and never shut down -- connections outlive any
/// individual window, exactly like the Tauri build's shared `AppState` did.
fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("pulseterm-ssh")
            .build()
            .expect("failed to start the SSH runtime")
    })
}

/// Runs `fut` on the SSH runtime and resolves on the caller's executor.
///
/// Awaiting the returned future is optional: dropping it cancels only the
/// waiting, not the work, which is what we want for fire-and-forget writes.
pub fn spawn<F, T>(fut: F) -> impl std::future::Future<Output = T>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = oneshot::channel();
    runtime().spawn(async move {
        let _ = tx.send(fut.await);
    });
    async move { rx.await.expect("SSH runtime task was dropped") }
}

/// Spawns work on the SSH runtime with no way to observe its result. Used for
/// the long-lived per-channel pump tasks.
pub fn detach<F>(fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    runtime().spawn(fut);
}
