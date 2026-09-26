//! The tokio runtime shared by the engine manager and the C ABI, and a guarded `block_on`.

use std::future::Future;

use tokio::runtime::Runtime;

use crate::error::{FfiError, Result};

/// Worker threads of an FFI tokio runtime (networking only; audio runs on dedicated threads
/// inside the engines).
pub const RUNTIME_WORKERS: usize = 2;

/// Builds a multi-thread runtime named `name`.
pub(crate) fn build_runtime(name: &str) -> Result<Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(RUNTIME_WORKERS)
        .thread_name(name)
        .enable_all()
        .build()
        .map_err(|e| FfiError::Internal(format!("cannot create the tokio runtime: {e}")))
}

/// Drives `fut` to completion on `runtime` from a synchronous caller.
///
/// # Errors
/// [`FfiError::Internal`] when called from inside a tokio runtime (blocking there would
/// panic).
pub(crate) fn block_on<F: Future>(runtime: &Runtime, fut: F) -> Result<F::Output> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(FfiError::Internal(
            "blocking engine call made from an async context".to_owned(),
        ));
    }
    Ok(runtime.block_on(fut))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_on_refuses_async_context() {
        let outer = build_runtime("hfa-test-outer").expect("runtime");
        let inner = outer.block_on(async {
            let rt = build_runtime("hfa-test-inner").expect("runtime");
            let r = block_on(&rt, async { 1 });
            // Dropping a runtime inside an async context panics; shut it down in the
            // background instead.
            rt.shutdown_background();
            r
        });
        assert!(matches!(inner, Err(FfiError::Internal(_))));
        assert_eq!(block_on(&outer, async { 7 }).expect("sync caller"), 7);
    }
}
