//! External PCM feeds: native capture code outside Rust (Android `AudioRecord` via JNI, the
//! iOS ReplayKit broadcast extension via the C ABI) pushes PCM into a global registry.
//!
//! Flow:
//! 1. Native side (through `hfa-ffi`) calls [`register_external`]`(id, format)` and keeps the
//!    returned [`ExternalFeed`].
//! 2. The sender opens [`crate::CaptureTarget::External`]`{ id }` via [`crate::open_capture`],
//!    which looks the feed up and returns an [`ExternalSource`].
//! 3. While the source is started, [`ExternalFeed::push`] writes into its ring; before
//!    `start` / after `stop`, pushed samples are dropped (and `push` returns 0).
//! 4. [`unregister_external`]`(id)` removes the feed.
//!
//! `push` may be called from any thread (it takes a short, uncontended `parking_lot` lock
//! around the SPSC producer; the native capture threads are not real-time callbacks).
//!
//! At most one [`ExternalSource`] can be attached to a feed at a time; starting a second one
//! fails with [`CaptureError::AlreadyRunning`]. Re-registering an id with the **same format**
//! returns a handle to the existing feed (a running source keeps receiving, e.g. when the
//! Android capture service restarts its `AudioRecord`); with a **different format** it
//! replaces the feed and detaches the source running on the old one (its format would be
//! wrong), so the sender has to re-open the target.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use hfa_audio::AudioFormat;
use parking_lot::Mutex;

use crate::ring::PcmSink;
use crate::{CaptureError, CaptureSource, Result};

/// Shared state between an [`ExternalFeed`] and its [`ExternalSource`].
struct FeedShared {
    id: u32,
    format: AudioFormat,
    /// `false` once the feed was unregistered (or replaced by a new registration).
    registered: AtomicBool,
    /// The attached sink and the token of the [`ExternalSource`] that attached it.
    sink: Mutex<Option<(u64, PcmSink)>>,
}

type Registry = Mutex<HashMap<u32, Arc<FeedShared>>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Source tokens: identify which [`ExternalSource`] attached the sink of a feed.
fn next_token() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Handle used by native code to push PCM. Cheap to clone; `Send + Sync`.
#[derive(Clone)]
pub struct ExternalFeed {
    shared: Arc<FeedShared>,
}

impl ExternalFeed {
    /// The feed id.
    pub fn id(&self) -> u32 {
        self.shared.id
    }

    /// The format pushed samples must have (fixed at registration).
    pub fn format(&self) -> AudioFormat {
        self.shared.format
    }

    /// Whether an [`ExternalSource`] is currently started on this feed (i.e. `push` delivers).
    pub fn is_attached(&self) -> bool {
        self.shared.sink.lock().is_some()
    }

    /// Pushes interleaved samples in [`ExternalFeed::format`]. Returns the number of samples
    /// accepted (0 if no source is currently started; less than `interleaved.len()` if the
    /// ring is full, see [`PcmSink::push`]).
    ///
    /// The feed does not convert: callers whose native format can differ from the registered
    /// one (e.g. ReplayKit buffers, whose rate/channels are only known per buffer) register
    /// the feed as [`AudioFormat::INTERNAL`] and convert each buffer first (see `hfa-ffi`'s
    /// `hfa_ext_push_pcm`).
    pub fn push(&self, interleaved: &[f32]) -> usize {
        match self.shared.sink.lock().as_mut() {
            Some((_, sink)) => sink.push(interleaved),
            None => 0,
        }
    }
}

impl std::fmt::Debug for ExternalFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalFeed")
            .field("id", &self.shared.id)
            .field("format", &self.shared.format)
            .field(
                "registered",
                &self.shared.registered.load(Ordering::Relaxed),
            )
            .finish()
    }
}

/// Registers (or replaces) feed `id` with the given format and returns its handle.
///
/// If feed `id` is already registered with the same format, a handle to that feed is
/// returned (an attached source stays attached). With a different format the old feed is
/// replaced and any source running on it is detached.
pub fn register_external(id: u32, format: AudioFormat) -> ExternalFeed {
    let mut map = registry().lock();
    if let Some(existing) = map.get(&id) {
        if existing.format == format {
            return ExternalFeed {
                shared: Arc::clone(existing),
            };
        }
    }
    let shared = Arc::new(FeedShared {
        id,
        format,
        registered: AtomicBool::new(true),
        sink: Mutex::new(None),
    });
    let old = map.insert(id, Arc::clone(&shared));
    drop(map);
    if let Some(old) = old {
        retire(&old);
    }
    tracing::debug!(id, ?format, "registered external feed");
    ExternalFeed { shared }
}

/// Removes feed `id`. A running [`ExternalSource`] for it stops receiving samples. Unknown
/// ids are ignored.
pub fn unregister_external(id: u32) {
    let old = registry().lock().remove(&id);
    if let Some(old) = old {
        retire(&old);
        tracing::debug!(id, "unregistered external feed");
    }
}

/// Marks a feed as gone and detaches (drops) its sink.
fn retire(feed: &FeedShared) {
    feed.registered.store(false, Ordering::Release);
    let sink = feed.sink.lock().take();
    drop(sink);
}

/// The [`CaptureSource`] side of a registered feed.
pub struct ExternalSource {
    shared: Arc<FeedShared>,
    token: u64,
}

impl ExternalSource {
    /// Looks up feed `id`.
    ///
    /// # Errors
    /// [`CaptureError::NotFound`] if no feed with this id is registered.
    pub fn open(id: u32) -> Result<Self> {
        let shared = registry()
            .lock()
            .get(&id)
            .cloned()
            .ok_or_else(|| CaptureError::NotFound(format!("external feed {id}")))?;
        Ok(Self {
            shared,
            token: next_token(),
        })
    }
}

impl CaptureSource for ExternalSource {
    fn describe(&self) -> String {
        format!("External feed {}", self.shared.id)
    }

    fn format(&self) -> AudioFormat {
        self.shared.format
    }

    fn start(&mut self, sink: PcmSink) -> Result<()> {
        let mut slot = self.shared.sink.lock();
        // Checked under the sink lock, so a concurrent `retire` either sees our sink (and
        // drops it) or we see `registered == false`.
        if !self.shared.registered.load(Ordering::Acquire) {
            return Err(CaptureError::NotFound(format!(
                "external feed {} was unregistered",
                self.shared.id
            )));
        }
        if slot.is_some() {
            return Err(CaptureError::AlreadyRunning);
        }
        *slot = Some((self.token, sink));
        Ok(())
    }

    fn stop(&mut self) {
        let mut slot = self.shared.sink.lock();
        if matches!(slot.as_ref(), Some((token, _)) if *token == self.token) {
            *slot = None;
        }
    }
}

impl Drop for ExternalSource {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring::pcm_ring_with_channels;

    // The registry is global and tests run in parallel: every test uses its own ids.

    #[test]
    fn feed_attach_detach() {
        let feed = register_external(1001, AudioFormat::new(44_100, 2));
        assert_eq!(feed.id(), 1001);
        assert_eq!(feed.format(), AudioFormat::new(44_100, 2));

        let mut src = ExternalSource::open(1001).expect("open");
        assert_eq!(src.format(), AudioFormat::new(44_100, 2));
        assert_eq!(src.describe(), "External feed 1001");

        // Not started: data is dropped.
        assert_eq!(feed.push(&[0.1, 0.2]), 0);
        assert!(!feed.is_attached());

        let (sink, mut ring) = pcm_ring_with_channels(8, 2);
        src.start(sink).expect("start");
        assert!(feed.is_attached());
        // From another thread, through a clone.
        let pusher = feed.clone();
        let pushed = std::thread::spawn(move || pusher.push(&[0.1, 0.2, 0.3, 0.4]))
            .join()
            .expect("join");
        assert_eq!(pushed, 4);
        assert_eq!(feed.push(&[0.5, 0.6, 0.7, 0.8, 0.9, 1.0]), 4, "ring full");
        let mut out = [0.0; 8];
        assert_eq!(ring.pull(&mut out), 8);
        assert_eq!(out, [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8]);

        src.stop();
        assert!(!feed.is_attached());
        assert_eq!(feed.push(&[0.1, 0.2]), 0, "stopped: dropped");
        assert_eq!(ring.available(), 0);

        // Restart with a new ring.
        let (sink, ring2) = pcm_ring_with_channels(8, 2);
        src.start(sink).expect("restart");
        assert_eq!(feed.push(&[0.1, 0.2]), 2);
        assert_eq!(ring2.available(), 2);
        drop(src); // Drop detaches.
        assert_eq!(feed.push(&[0.1, 0.2]), 0);
        unregister_external(1001);
    }

    #[test]
    fn open_through_capture_target() {
        let feed = register_external(1002, AudioFormat::INTERNAL);
        let mut src =
            crate::open_capture(&crate::CaptureTarget::External { id: 1002 }).expect("open");
        assert_eq!(src.format(), AudioFormat::INTERNAL);
        let (sink, ring) = pcm_ring_with_channels(16, 2);
        src.start(sink).expect("start");
        assert_eq!(feed.push(&[0.0; 4]), 4);
        assert_eq!(ring.available(), 4);
        unregister_external(1002);
    }

    #[test]
    fn unknown_and_unregistered_feeds() {
        assert!(matches!(
            ExternalSource::open(1003),
            Err(CaptureError::NotFound(_))
        ));
        let feed = register_external(1003, AudioFormat::INTERNAL);
        let mut running = ExternalSource::open(1003).expect("open");
        let mut idle = ExternalSource::open(1003).expect("open");
        let (sink, ring) = pcm_ring_with_channels(16, 2);
        running.start(sink).expect("start");
        assert_eq!(feed.push(&[0.0; 2]), 2);

        unregister_external(1003);
        unregister_external(1003); // unknown ids are ignored
        assert_eq!(feed.push(&[0.0; 2]), 0, "unregistered: detached");
        assert_eq!(ring.available(), 2);
        assert!(matches!(
            ExternalSource::open(1003),
            Err(CaptureError::NotFound(_))
        ));
        let (sink, _ring) = pcm_ring_with_channels(16, 2);
        assert!(matches!(idle.start(sink), Err(CaptureError::NotFound(_))));
        running.stop();
    }

    #[test]
    fn only_one_source_per_feed() {
        let feed = register_external(1004, AudioFormat::INTERNAL);
        let mut a = ExternalSource::open(1004).expect("open");
        let mut b = ExternalSource::open(1004).expect("open");
        let (sink_a, ring_a) = pcm_ring_with_channels(16, 2);
        let (sink_b, _ring_b) = pcm_ring_with_channels(16, 2);
        a.start(sink_a).expect("start a");
        assert_eq!(b.start(sink_b).err(), Some(CaptureError::AlreadyRunning));
        // Stopping the source that is not attached must not detach the other one.
        b.stop();
        assert_eq!(feed.push(&[0.0; 2]), 2);
        assert_eq!(ring_a.available(), 2);
        drop(b);
        assert!(feed.is_attached());
        a.stop();
        unregister_external(1004);
    }

    #[test]
    fn re_registering_with_the_same_format_keeps_the_source_attached() {
        let first = register_external(1006, AudioFormat::INTERNAL);
        let mut src = ExternalSource::open(1006).expect("open");
        let (sink, ring) = pcm_ring_with_channels(16, 2);
        src.start(sink).expect("start");
        let second = register_external(1006, AudioFormat::INTERNAL);
        assert_eq!(second.push(&[0.0; 2]), 2);
        assert_eq!(first.push(&[0.0; 2]), 2);
        assert_eq!(ring.available(), 4);
        src.stop();
        unregister_external(1006);
    }

    #[test]
    fn re_registering_with_another_format_replaces_the_feed() {
        let old = register_external(1005, AudioFormat::new(16_000, 1));
        let mut old_src = ExternalSource::open(1005).expect("open");
        let (sink, _ring) = pcm_ring_with_channels(16, 1);
        old_src.start(sink).expect("start");

        let new = register_external(1005, AudioFormat::INTERNAL);
        assert_eq!(old.push(&[0.0]), 0, "old feed detached by the replacement");
        let new_src = ExternalSource::open(1005).expect("open");
        assert_eq!(new_src.format(), AudioFormat::INTERNAL);
        assert_eq!(new.format(), AudioFormat::INTERNAL);
        unregister_external(1005);
    }
}
