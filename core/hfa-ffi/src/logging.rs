//! Log setup.
//!
//! - Desktop: a `tracing-subscriber` `fmt` subscriber on stderr, filtered by `RUST_LOG`
//!   (default `info`), installed once by [`init_tracing`] (from `init_app` or the C ABI).
//! - Android / iOS: no subscriber. `tracing` is built with its `log` feature there, so events
//!   become `log` records, and [`init_platform_logger`] (flutter_rust_bridge init hook, or the
//!   C ABI) installs flutter_rust_bridge's console logger that forwards them to logcat /
//!   os_log (needs the `flutter` feature, on by default).
//! - The iOS broadcast extension (C ABI, built without `flutter`): no console logger is
//!   available there, so [`init_ext_logging`] writes a `tracing-subscriber` `fmt` log to
//!   `<data_dir>/`[`EXT_LOG_FILE`] in the App Group, capped at [`EXT_LOG_MAX_BYTES`] (the
//!   previous part is kept as `broadcast.log.1`). The app can read it; `Console.app` shows the
//!   Swift side's `os.Logger` messages.

use std::path::Path;
use std::sync::Once;

/// Log file of the iOS broadcast extension inside its data directory.
#[cfg(any(test, all(target_os = "ios", not(feature = "flutter"))))]
pub(crate) const EXT_LOG_FILE: &str = "broadcast.log";
/// Size at which the extension's log file is rotated (one previous part is kept).
#[cfg(all(target_os = "ios", not(feature = "flutter")))]
pub(crate) const EXT_LOG_MAX_BYTES: u64 = 256 * 1024;

/// Default filter when `RUST_LOG` is not set (desktop).
#[cfg(not(any(target_os = "android", target_os = "ios")))]
const DEFAULT_LOG_FILTER: &str = "info";

/// Installs the process-wide log pipeline (idempotent, never fails: if another subscriber
/// or logger is already installed, that one is kept).
#[cfg_attr(all(target_os = "ios", not(feature = "flutter")), allow(dead_code))]
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

/// Log setup for the C ABI (the broadcast extension): a capped log file in `data_dir` on iOS
/// builds without the `flutter` feature, [`init_tracing`] everywhere else. Idempotent (the
/// first call wins); never fails (without a writable file nothing is logged).
pub(crate) fn init_ext_logging(data_dir: &Path) {
    #[cfg(all(target_os = "ios", not(feature = "flutter")))]
    {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let Ok(file) = capped::CappedLog::open(&data_dir.join(EXT_LOG_FILE), EXT_LOG_MAX_BYTES)
            else {
                return;
            };
            let filter = tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
                .try_init();
        });
    }
    #[cfg(not(all(target_os = "ios", not(feature = "flutter"))))]
    {
        let _ = data_dir;
        init_tracing();
    }
}

/// Routes `log` records to the platform console on Android (logcat) and iOS (os_log).
/// No-op elsewhere. Idempotent.
#[cfg_attr(all(target_os = "ios", not(feature = "flutter")), allow(dead_code))]
pub(crate) fn init_platform_logger() {
    #[cfg(all(feature = "flutter", any(target_os = "android", target_os = "ios")))]
    {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            flutter_rust_bridge::setup_log_to_console(log::LevelFilter::Info);
        });
    }
}

/// A size-capped, append-only log file.
#[cfg(any(test, all(target_os = "ios", not(feature = "flutter"))))]
mod capped {
    use std::fs::{File, OpenOptions};
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};

    /// Appends to `path`; once `max` bytes are reached, the file becomes `<path>.1`
    /// (replacing an older one) and a new file is started.
    pub(crate) struct CappedLog {
        path: PathBuf,
        file: Option<File>,
        written: u64,
        max: u64,
    }

    impl CappedLog {
        pub(crate) fn open(path: &Path, max: u64) -> io::Result<Self> {
            let file = OpenOptions::new().create(true).append(true).open(path)?;
            let written = file.metadata()?.len();
            Ok(Self {
                path: path.to_path_buf(),
                file: Some(file),
                written,
                max,
            })
        }

        pub(crate) fn previous_path(path: &Path) -> PathBuf {
            let mut name = path.as_os_str().to_owned();
            name.push(".1");
            PathBuf::from(name)
        }

        fn rotate(&mut self) -> io::Result<()> {
            self.file = None;
            std::fs::rename(&self.path, Self::previous_path(&self.path))?;
            self.file = Some(
                OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&self.path)?,
            );
            self.written = 0;
            Ok(())
        }
    }

    impl Write for CappedLog {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.written >= self.max || self.file.is_none() {
                self.rotate()?;
            }
            let file = self.file.as_mut().ok_or(io::ErrorKind::NotFound)?;
            file.write_all(buf)?;
            self.written += buf.len() as u64;
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.file.as_mut().map_or(Ok(()), Write::flush)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_log_is_capped_and_keeps_one_previous_part() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join(crate::logging::EXT_LOG_FILE);
            std::fs::write(&path, b"from an earlier run\n").expect("seed");
            let mut log = CappedLog::open(&path, 50).expect("open");
            // Appends to what is there.
            log.write_all(b"0123456789012345678901234567890123456789\n")
                .expect("write");
            assert!(std::fs::read_to_string(&path)
                .expect("read")
                .starts_with("from an earlier run"));
            // Over the cap: the next line starts a new file.
            log.write_all(b"next\n").expect("write");
            log.flush().expect("flush");
            assert_eq!(std::fs::read_to_string(&path).expect("read"), "next\n");
            let previous = CappedLog::previous_path(&path);
            assert!(std::fs::read_to_string(previous)
                .expect("previous part")
                .contains("0123456789"));
            // A file that is already over the cap is rotated on the first write.
            let mut again = CappedLog::open(&path, 2).expect("reopen");
            again.write_all(b"x").expect("write");
            assert_eq!(std::fs::read_to_string(&path).expect("read"), "x");
        }
    }
}
