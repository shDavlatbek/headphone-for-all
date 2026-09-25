//! External PCM feeds owned by the app: registered by `sender_start` for
//! [`crate::api::sender::CaptureSourceDto::External`], fed by the Android JNI exports.
//!
//! [`hfa_capture::register_external`] has no lookup, so this module keeps the
//! [`ExternalFeed`] handles by id. A push for an id that was never registered here (or was
//! already unregistered) is reported as [`HFA_ERR_UNKNOWN_FEED`] instead of silently
//! creating a feed nobody reads.

// Registration is driven by the flutter_rust_bridge API; without the `flutter` feature only
// the JNI side (or nothing) remains.
#![cfg_attr(not(feature = "flutter"), allow(dead_code))]

use std::collections::HashMap;
use std::sync::OnceLock;

use hfa_audio::AudioFormat;
use hfa_capture::ExternalFeed;
use parking_lot::RwLock;

#[cfg(any(target_os = "android", test))]
use crate::c_api::{HFA_ERR_INVALID_ARGUMENT, HFA_ERR_UNKNOWN_FEED, HFA_OK};
#[cfg(any(target_os = "android", test))]
use crate::pcm::{format_is_valid, MAX_SAMPLES_PER_CALL};

type Feeds = RwLock<HashMap<u32, ExternalFeed>>;

fn feeds() -> &'static Feeds {
    static FEEDS: OnceLock<Feeds> = OnceLock::new();
    FEEDS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Registers feed `id` with `format` in `hfa-capture` and remembers its handle.
pub(crate) fn register(id: u32, format: AudioFormat) -> ExternalFeed {
    let feed = hfa_capture::register_external(id, format);
    feeds().write().insert(id, feed.clone());
    feed
}

/// Forgets feed `id` and unregisters it from `hfa-capture`. Unknown ids are ignored.
pub(crate) fn unregister(id: u32) {
    if feeds().write().remove(&id).is_some() {
        hfa_capture::unregister_external(id);
    }
}

/// The registered handle of feed `id`.
#[cfg(any(target_os = "android", test))]
pub(crate) fn get(id: u32) -> Option<ExternalFeed> {
    feeds().read().get(&id).cloned()
}

/// Pushes interleaved `samples` (`channels` × `sample_rate` Hz, which must equal the
/// registered format) into feed `id`. Returns an `HFA_*` code:
/// [`HFA_OK`] (also when no source is attached yet: the samples are dropped),
/// [`HFA_ERR_UNKNOWN_FEED`], or [`HFA_ERR_INVALID_ARGUMENT`] (bad or mismatching format,
/// oversized buffer).
#[cfg(any(target_os = "android", test))]
pub(crate) fn push(id: u32, channels: i32, sample_rate: i32, samples: &[f32]) -> i32 {
    let (Ok(channels), Ok(rate)) = (u32::try_from(channels), u32::try_from(sample_rate)) else {
        return HFA_ERR_INVALID_ARGUMENT;
    };
    if !format_is_valid(channels, rate) || samples.len() > MAX_SAMPLES_PER_CALL {
        return HFA_ERR_INVALID_ARGUMENT;
    }
    let Some(feed) = get(id) else {
        return HFA_ERR_UNKNOWN_FEED;
    };
    let format = feed.format();
    if u32::from(format.channels) != channels || format.sample_rate != rate {
        return HFA_ERR_INVALID_ARGUMENT;
    }
    feed.push(samples);
    HFA_OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use hfa_capture::{open_capture, pcm_ring_with_channels, CaptureTarget};

    // Feed ids are process-global: each test uses its own.

    #[test]
    fn unknown_feed_is_reported() {
        assert_eq!(
            push(0xF00D_0001, 2, 48_000, &[0.0; 4]),
            HFA_ERR_UNKNOWN_FEED
        );
    }

    #[test]
    fn format_is_checked() {
        let id = 0xF00D_0002;
        register(id, AudioFormat::new(44_100, 1));
        assert_eq!(push(id, 2, 44_100, &[0.0; 4]), HFA_ERR_INVALID_ARGUMENT);
        assert_eq!(push(id, 1, 48_000, &[0.0; 4]), HFA_ERR_INVALID_ARGUMENT);
        assert_eq!(push(id, -1, 44_100, &[0.0; 4]), HFA_ERR_INVALID_ARGUMENT);
        assert_eq!(push(id, 1, 0, &[0.0; 4]), HFA_ERR_INVALID_ARGUMENT);
        let huge = vec![0.0; MAX_SAMPLES_PER_CALL + 1];
        assert_eq!(push(id, 1, 44_100, &huge), HFA_ERR_INVALID_ARGUMENT);
        // Not attached yet: accepted and dropped.
        assert_eq!(push(id, 1, 44_100, &[0.0; 4]), HFA_OK);
        unregister(id);
        assert_eq!(push(id, 1, 44_100, &[0.0; 4]), HFA_ERR_UNKNOWN_FEED);
        unregister(id); // idempotent
    }

    #[test]
    fn pushed_samples_reach_the_capture_source() {
        let id = 0xF00D_0003;
        let format = AudioFormat::new(48_000, 2);
        register(id, format);
        let mut source = open_capture(&CaptureTarget::External { id }).expect("open");
        assert_eq!(source.format(), format);
        let (sink, mut ring) = pcm_ring_with_channels(4_800, 2);
        source.start(sink).expect("start");
        assert!(get(id).expect("registered").is_attached());
        assert_eq!(push(id, 2, 48_000, &[0.5, -0.5, 0.25, -0.25]), HFA_OK);
        let mut out = [0.0_f32; 4];
        assert_eq!(ring.pull(&mut out), 4);
        assert_eq!(out, [0.5, -0.5, 0.25, -0.25]);
        source.stop();
        unregister(id);
    }
}
