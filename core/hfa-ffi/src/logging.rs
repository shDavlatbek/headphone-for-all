//! Log setup.
//!
//! - Desktop: a `tracing-subscriber` `fmt` subscriber on stderr, filtered by `RUST_LOG`
//!   (default `info`), installed once by [`init_tracing`] (from `init_app` or the C ABI).
//! - Android / iOS: no subscriber. `tracing` is built with its `log` feature there, so events
//!   become `log` records, and [`init_platform_logger`] (flutter_rust_bridge init hook, or the
//!   C ABI) installs flutter_rust_bridge's console logger that forwards them to logcat /
//!   os_log (needs the `flutter` feature, on by default).

use std::sync::Once;

/// Default filter when `RUST_LOG` is not set (desktop).
#[cfg(not(any(target_os = "android", target_os = "ios")))]
const DEFAULT_LOG_FILTER: &str = "info";

/// Installs the process-wide log pipeline (idempotent, never fails: if another subscriber
/// or logger is already installed, that one is kept).
pub(crate) fn init_tracing() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        init_platform_logger();
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            let filter = tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(DEFAULT_LOG_FILTER));
            // Fails only if a global subscriber exists already (e.g. a host process set one).
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .try_init();
        }
    });
}

/// Routes `log` records to the platform console on Android (logcat) and iOS (os_log).
/// No-op elsewhere. Idempotent.
pub(crate) fn init_platform_logger() {
    #[cfg(all(feature = "flutter", any(target_os = "android", target_os = "ios")))]
    {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            flutter_rust_bridge::setup_log_to_console(log::LevelFilter::Info);
        });
    }
}
