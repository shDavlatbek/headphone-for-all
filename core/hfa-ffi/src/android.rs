//! JNI exports for the Android app (`io.github.shdavlatbek.hfa.NativeBridge`): the process
//! setup (`init`) and the capture service's PCM pushes.
//!
//! `NativeBridge.init(applicationContext)` must run once per process before Dart opens an audio
//! output (`hub_start`, `list_output_devices`): cpal's AAudio backend reads the `JavaVM` and an
//! `android.content.Context` from [`ndk_context`], which nothing else initializes in a Flutter
//! app. The app calls it from `HfaApplication.onCreate`. Without it those calls fail with
//! [`FfiError::Internal`] (see [`ensure_audio_context`]) instead of panicking in cpal.
//!
//! The Kotlin `CaptureService` (MediaProjection + `AudioRecord` with
//! `AudioPlaybackCaptureConfiguration`) pushes PCM for the feed id that Dart passed to
//! `sender_start(CaptureSourceDto::External { feed_id, sample_rate, channels })`:
//!
//! ```kotlin
//! object NativeBridge {
//!     init { System.loadLibrary("hfa_ffi") }
//!     external fun init(context: Context): Int
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
use std::sync::atomic::{AtomicBool, Ordering};

use jni::objects::{JFloatArray, JObject, JShortArray};
use jni::sys::jint;
use jni::{EnvUnowned, Outcome};
use parking_lot::Mutex;

use crate::c_api::{HFA_ERR_ENGINE, HFA_ERR_INTERNAL, HFA_ERR_INVALID_ARGUMENT, HFA_OK};
use crate::error::{FfiError, Result};
use crate::feeds;
use crate::pcm::MAX_SAMPLES_PER_CALL;

/// Serializes `NativeBridge.init` calls: [`ndk_context::initialize_android_context`] must run
/// at most once per process (it asserts that).
static CONTEXT_INIT: Mutex<()> = Mutex::new(());

/// `true` once [`ndk_context`] holds the `JavaVM` and the application context.
static CONTEXT_READY: AtomicBool = AtomicBool::new(false);

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
            tracing::warn!("NativeBridge call failed: {e}");
            HFA_ERR_ENGINE
        }
        Outcome::Panic(_) => HFA_ERR_INTERNAL,
    }
}

/// Fails unless `NativeBridge.init` set the Android context that cpal's AAudio output needs
/// (checked before opening an output, so a missing call is an error instead of a cpal panic).
pub(crate) fn ensure_audio_context() -> Result<()> {
    if CONTEXT_READY.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(FfiError::Internal(
            "the Android context is not initialized (NativeBridge.init was not called)".to_owned(),
        ))
    }
}

/// `NativeBridge.init(context: Context): Int`: stores the `JavaVM` and a global reference to
/// `context` (the application context: it lives as long as the process) in [`ndk_context`]
/// for cpal's AAudio backend.
///
/// Idempotent: later calls return `0` without changing anything. Returns `-1` for a `null`
/// context, `-3` on a JNI failure, `-5` on a caught panic. The global reference is never
/// deleted (the context lives as long as the process).
#[no_mangle]
pub extern "system" fn Java_io_github_shdavlatbek_hfa_NativeBridge_init<'caller>(
    mut env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    context: JObject<'caller>,
) -> jint {
    let outcome = env.with_env(|env| -> jni::errors::Result<jint> {
        if context.is_null() {
            return Ok(HFA_ERR_INVALID_ARGUMENT);
        }
        let _guard = CONTEXT_INIT.lock();
        if CONTEXT_READY.load(Ordering::Acquire) {
            return Ok(HFA_OK);
        }
        let vm = env.get_java_vm()?;
        let global = env.new_global_ref(&context)?;
        // SAFETY: both pointers stay valid for the life of the process: the JavaVM is never
        // destroyed on Android and the global reference is leaked on purpose (`into_raw`).
        // `CONTEXT_INIT` + `CONTEXT_READY` make this the only call in the process.
        unsafe {
            ndk_context::initialize_android_context(vm.get_raw().cast(), global.into_raw().cast());
        }
        CONTEXT_READY.store(true, Ordering::Release);
        tracing::info!("Android context initialized for audio output");
        Ok(HFA_OK)
    });
    resolve(outcome.into_outcome())
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
