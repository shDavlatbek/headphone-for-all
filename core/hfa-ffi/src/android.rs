//! JNI exports for the Android capture service (`io.github.shdavlatbek.hfa.NativeBridge`).
//!
//! The Kotlin `CaptureService` (MediaProjection + `AudioRecord` with
//! `AudioPlaybackCaptureConfiguration`) pushes PCM for the feed id that Dart passed to
//! `sender_start(CaptureSourceDto::External { feed_id, sample_rate, channels })`:
//!
//! ```kotlin
//! object NativeBridge {
//!     init { System.loadLibrary("hfa_ffi") }
//!     external fun pushPcm(feedId: Int, data: FloatArray, frames: Int, channels: Int, sampleRate: Int): Int
//!     external fun pushPcm16(feedId: Int, data: ShortArray, frames: Int, channels: Int, sampleRate: Int): Int
//! }
//! ```
//!
//! Both return an `HFA_*` code ([`crate::c_api`]): `0` ok (also while the sender is still
//! connecting), `-1` invalid argument (format differs from the registered one, array shorter
//! than `frames * channels`, negative sizes), `-4` unknown feed id (before `sender_start` or
//! after `sender_stop`: drop the audio), `-3` JNI failure, `-5` caught panic. They never
//! throw and never unwind into the JVM. Each calling thread reuses its own sample buffers,
//! so steady-state pushes do not allocate.

use std::cell::RefCell;

use jni::objects::{JFloatArray, JObject, JShortArray};
use jni::sys::jint;
use jni::{EnvUnowned, Outcome};

use crate::c_api::{HFA_ERR_ENGINE, HFA_ERR_INTERNAL, HFA_ERR_INVALID_ARGUMENT, HFA_OK};
use crate::feeds;
use crate::pcm::MAX_SAMPLES_PER_CALL;

thread_local! {
    static F32_BUF: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };
    static I16_BUF: RefCell<Vec<i16>> = const { RefCell::new(Vec::new()) };
}

/// Number of samples of a push, or `None` if negative / too large.
fn sample_count(frames: jint, channels: jint) -> Option<usize> {
    let frames = usize::try_from(frames).ok()?;
    let channels = usize::try_from(channels).ok()?;
    frames
        .checked_mul(channels)
        .filter(|n| *n <= MAX_SAMPLES_PER_CALL)
}

/// Maps a `with_env` outcome to a return code.
fn resolve(outcome: Outcome<jint, jni::errors::Error>) -> jint {
    match outcome {
        Outcome::Ok(code) => code,
        Outcome::Err(e) => {
            tracing::warn!("NativeBridge push failed: {e}");
            HFA_ERR_ENGINE
        }
        Outcome::Panic(_) => HFA_ERR_INTERNAL,
    }
}

/// `NativeBridge.pushPcm(feedId, data: FloatArray, frames, channels, sampleRate): Int`.
#[no_mangle]
pub extern "system" fn Java_io_github_shdavlatbek_hfa_NativeBridge_pushPcm<'caller>(
    mut env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    feed_id: jint,
    data: JFloatArray<'caller>,
    frames: jint,
    channels: jint,
    sample_rate: jint,
) -> jint {
    let outcome = env.with_env(|env| -> jni::errors::Result<jint> {
        let Some(n) = sample_count(frames, channels) else {
            return Ok(HFA_ERR_INVALID_ARGUMENT);
        };
        if n == 0 {
            return Ok(HFA_OK);
        }
        if data.len(env)? < n {
            return Ok(HFA_ERR_INVALID_ARGUMENT);
        }
        F32_BUF.with(|buf| {
            let mut buf = buf.borrow_mut();
            buf.resize(n, 0.0);
            data.get_region(env, 0, &mut buf[..])?;
            // The feed id is an opaque 32-bit value shared with Dart; keep its bits.
            Ok(feeds::push(feed_id as u32, channels, sample_rate, &buf[..]))
        })
    });
    resolve(outcome.into_outcome())
}

/// `NativeBridge.pushPcm16(feedId, data: ShortArray, frames, channels, sampleRate): Int`
/// (16-bit PCM, converted to `f32`).
#[no_mangle]
pub extern "system" fn Java_io_github_shdavlatbek_hfa_NativeBridge_pushPcm16<'caller>(
    mut env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    feed_id: jint,
    data: JShortArray<'caller>,
    frames: jint,
    channels: jint,
    sample_rate: jint,
) -> jint {
    let outcome = env.with_env(|env| -> jni::errors::Result<jint> {
        let Some(n) = sample_count(frames, channels) else {
            return Ok(HFA_ERR_INVALID_ARGUMENT);
        };
        if n == 0 {
            return Ok(HFA_OK);
        }
        if data.len(env)? < n {
            return Ok(HFA_ERR_INVALID_ARGUMENT);
        }
        I16_BUF.with(|ibuf| {
            F32_BUF.with(|fbuf| {
                let (mut ibuf, mut fbuf) = (ibuf.borrow_mut(), fbuf.borrow_mut());
                ibuf.resize(n, 0);
                data.get_region(env, 0, &mut ibuf[..])?;
                fbuf.resize(n, 0.0);
                hfa_audio::convert::i16_to_f32(&ibuf[..], &mut fbuf[..]);
                Ok(feeds::push(
                    feed_id as u32,
                    channels,
                    sample_rate,
                    &fbuf[..],
                ))
            })
        })
    });
    resolve(outcome.into_outcome())
}
