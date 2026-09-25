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

use std::sync::Arc;

use hfa_audio::AudioFormat;

use crate::ring::PcmSink;
use crate::{CaptureSource, Result};

/// Shared state between an [`ExternalFeed`] and its [`ExternalSource`].
struct FeedShared {
    id: u32,
    format: AudioFormat,
    sink: parking_lot::Mutex<Option<PcmSink>>,
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

    /// Pushes interleaved samples in [`ExternalFeed::format`]. Returns the number of samples
    /// accepted (0 if no source is currently started; less than `interleaved.len()` if the
    /// ring is full).
    ///
    /// The feed does not convert: callers whose native format can differ from the registered
    /// one (e.g. ReplayKit buffers, whose rate/channels are only known per buffer) register
    /// the feed as [`AudioFormat::INTERNAL`] and convert each buffer first (see `hfa-ffi`'s
    /// `hfa_ext_push_pcm`).
    pub fn push(&self, _interleaved: &[f32]) -> usize {
        let _ = &self.shared.sink;
        todo!("feat/capture")
    }
}

/// Registers (or replaces) feed `id` with the given format and returns its handle.
pub fn register_external(_id: u32, _format: AudioFormat) -> ExternalFeed {
    todo!("feat/capture")
}

/// Removes feed `id`. A running [`ExternalSource`] for it stops receiving samples.
pub fn unregister_external(_id: u32) {
    todo!("feat/capture")
}

/// The [`CaptureSource`] side of a registered feed.
pub struct ExternalSource {
    shared: Arc<FeedShared>,
}

impl ExternalSource {
    /// Looks up feed `id`.
    ///
    /// # Errors
    /// [`crate::CaptureError::NotFound`] if no feed with this id is registered.
    pub fn open(_id: u32) -> Result<Self> {
        todo!("feat/capture")
    }
}

impl CaptureSource for ExternalSource {
    fn describe(&self) -> String {
        format!("External feed {}", self.shared.id)
    }

    fn format(&self) -> AudioFormat {
        self.shared.format
    }

    fn start(&mut self, _sink: PcmSink) -> Result<()> {
        todo!("feat/capture")
    }

    fn stop(&mut self) {
        todo!("feat/capture")
    }
}
