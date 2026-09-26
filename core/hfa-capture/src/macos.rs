//! macOS capture: Core Audio process taps (macOS 14.2+).
//!
//! How a capture is built (a Rust translation of insidegui/AudioCap `ProcessTap.swift`):
//!
//! 1. A [`CATapDescription`] describes what to tap: a stereo global tap that excludes a list of
//!    process objects (system mix; the list holds our own process: required when excluding
//!    self, and whenever Core Audio knows it for the plain system mix too, because our own
//!    playback is a hub's mix that must neither be sent back nor muted by the tap), or a
//!    stereo mixdown of one process object (per-app capture). It gets a fresh UUID, is private
//!    (only visible to this process) and uses [`MUTE_BEHAVIOR`].
//! 2. `AudioHardwareCreateProcessTap` turns the description into a tap `AudioObjectID` (its
//!    `kAudioTapPropertyFormat` is only logged for diagnostics).
//! 3. A private aggregate device is created whose only member is the tap (`TapList` entry with
//!    the tap's UUID and drift compensation, `TapAutoStart = 1`). Unlike AudioCap we do not add
//!    the default output device as a sub-device: a headset with a microphone would otherwise
//!    add its input streams to the aggregate's input buffers (same approach as `audiotee`).
//!    The capture format (`format()`) is the virtual format of the aggregate's first *input
//!    stream*, polled briefly until the new device exposes it: that is what the IOProc
//!    receives, and it can differ from the tap format (drift compensation resamples the tap
//!    to the aggregate's clock).
//! 4. An IOProc registered on the aggregate device receives the tapped audio as its *input*
//!    `AudioBufferList` and copies it into the [`PcmSink`] without allocating (interleaved or
//!    non-interleaved float32).
//!
//! `stop` tears it down in the reverse order: `AudioDeviceStop`, `AudioDeviceDestroyIOProcID`,
//! `AudioHardwareDestroyAggregateDevice`, `AudioHardwareDestroyProcessTap`.
//!
//! **Health.** The format is fixed when the source opens, so the device is watched with
//! property listeners ([`HealthWatch`]): the aggregate device dying
//! (`kAudioDevicePropertyDeviceIsAlive`), the audio server restarting
//! (`kAudioHardwarePropertyServiceRestarted`), and a change of the aggregate's nominal sample
//! rate or of its input stream's virtual format (the tap follows the output device, e.g. when
//! the default output switches to a 44.1 kHz device). Any of them makes
//! [`CaptureSource::error`] report it and the IOProc stop pushing (audio at another rate would
//! play at the wrong pitch); the owner opens a new source.
//!
//! **Availability.** `AudioHardwareCreateProcessTap`/`AudioHardwareDestroyProcessTap` and the
//! `CATapDescription` class only exist on macOS 14.2+. The two functions are resolved at run
//! time with `dlsym` (a direct reference would be a strong import that makes the whole binary
//! fail to launch on older macOS), and the class is looked up before use. On older systems every
//! entry point returns [`CaptureError::Unsupported`]; nothing panics.
//!
//! **Permission.** Taps need the "System Audio Recording" permission. The app bundle must carry
//! `NSAudioCaptureUsageDescription` in its `Info.plist`; macOS prompts on first use. If the user
//! denies it, some macOS versions fail `AudioHardwareCreateProcessTap` (mapped to
//! [`CaptureError::PermissionDenied`]) while others deliver silence. Elsewhere only `'!hog'`
//! means a permission problem; `'nope'` from other calls is a [`CaptureError::Backend`].
//!
//! This file is owned by `feat/capture-macos`. It exposes exactly the four `pub(crate)`
//! functions of the platform-module interface (see `docs/CONTRACTS.md` §5).

use std::ffi::{c_void, CStr};
use std::mem::{self, size_of};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use hfa_audio::AudioFormat;
use objc2::rc::Retained;
use objc2::runtime::AnyClass;
use objc2::AnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioDevicePermissionsError,
    kAudioDevicePropertyDeviceIsAlive, kAudioDevicePropertyNominalSampleRate,
    kAudioDevicePropertyStreams, kAudioHardwareIllegalOperationError,
    kAudioHardwarePropertyProcessObjectList, kAudioHardwarePropertyServiceRestarted,
    kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioHardwareUnsupportedOperationError,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectSystemObject, kAudioObjectUnknown,
    kAudioProcessPropertyBundleID, kAudioProcessPropertyIsRunningOutput, kAudioProcessPropertyPID,
    kAudioStreamPropertyVirtualFormat, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
    kAudioTapPropertyFormat, AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID,
    AudioDeviceIOProc, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareDestroyAggregateDevice,
    AudioObjectAddPropertyListener, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectID, AudioObjectPropertyAddress, AudioObjectPropertyScope,
    AudioObjectPropertySelector, AudioObjectRemovePropertyListener, CATapDescription,
    CATapMuteBehavior,
};
use objc2_core_audio_types::{
    kAudioFormatFlagIsBigEndian, kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved,
    kAudioFormatLinearPCM, AudioBuffer, AudioBufferList, AudioStreamBasicDescription,
    AudioTimeStamp,
};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_foundation::{NSArray, NSNumber, NSString, NSUUID};

use crate::{Capabilities, CaptureApp, CaptureError, CaptureSource, PcmSink};

/// What the tap does to the tapped audio on the local output device.
///
/// `CATapMutedWhenTapped`: the tapped processes keep playing on this Mac's speakers until the
/// tap is actually read; while we capture (the sender streams to the hub) the local output of
/// the tapped audio is silenced, so the sender's speakers go quiet and the audio is heard only
/// in the hub's headphone. When capture stops, local playback resumes by itself.
pub(crate) const MUTE_BEHAVIOR: CATapMuteBehavior = CATapMuteBehavior::MutedWhenTapped;

/// Minimum macOS version with Core Audio process taps, as `(major, minor)`.
const MIN_MACOS: (u16, u16) = (14, 2);

/// Frames interleaved per step when the tap delivers non-interleaved buffers. The scratch buffer
/// (`SCRATCH_FRAMES * channels` samples) is allocated in `start`, never in the IOProc.
const SCRATCH_FRAMES: usize = 1024;

/// Most channels handled for non-interleaved input (taps are stereo; this is headroom).
const MAX_PLANAR_CHANNELS: usize = 16;

/// How many times to look for the aggregate device's input stream format, and the pause between
/// tries: a freshly created aggregate device can take a moment before its tap stream exists.
const DEVICE_READY_ATTEMPTS: u32 = 20;
/// Pause between two [`DEVICE_READY_ATTEMPTS`].
const DEVICE_READY_DELAY: Duration = Duration::from_millis(25);

/// How many times to look up this process's Core Audio object before giving up on
/// `exclude_self`, and the pause between tries (the HAL registers a process on first contact).
const OWN_PROCESS_ATTEMPTS: u32 = 5;
/// Pause between two [`OWN_PROCESS_ATTEMPTS`].
const OWN_PROCESS_DELAY: Duration = Duration::from_millis(20);

/// Name given to the tap and to the private aggregate device.
const DEVICE_NAME: &str = "headphone-for-all capture";

/// Core Audio `OSStatus`.
type OsStatus = i32;

/// `noErr`.
const NO_ERR: OsStatus = 0;

/// An all-zero stream description (the struct has no `Default`).
const EMPTY_ASBD: AudioStreamBasicDescription = AudioStreamBasicDescription {
    mSampleRate: 0.0,
    mFormatID: 0,
    mFormatFlags: 0,
    mBytesPerPacket: 0,
    mFramesPerPacket: 0,
    mBytesPerFrame: 0,
    mChannelsPerFrame: 0,
    mBitsPerChannel: 0,
    mReserved: 0,
};

// ---------------------------------------------------------------------------------------------
// Platform-module interface
// ---------------------------------------------------------------------------------------------

/// Capture capabilities on macOS: system mix and per-app capture through Core Audio process taps
/// (macOS 14.2+). Capturing mutes the tapped audio on the local output ([`MUTE_BEHAVIOR`]).
pub(crate) fn capabilities() -> Capabilities {
    let permission =
        "Needs the \"System Audio Recording\" permission (System Settings > Privacy & \
                      Security); the app's Info.plist must contain NSAudioCaptureUsageDescription.";
    match ensure_supported() {
        Ok(_) => Capabilities {
            system_mix: true,
            per_app: true,
            mutes_local_output: true,
            notes: format!(
                "Core Audio process taps (macOS {}.{}+). {permission} While capturing, the tapped \
                 audio is muted on this Mac's own speakers (CATapMutedWhenTapped) and plays only \
                 in the hub's headphone. \"system\" also leaves out this app's own playback (a \
                 hub's mix) once Core Audio knows this app, like \"system-excl\".",
                MIN_MACOS.0, MIN_MACOS.1
            ),
        },
        Err(e) => Capabilities {
            system_mix: false,
            per_app: false,
            mutes_local_output: false,
            notes: format!("{e}. {permission}"),
        },
    }
}

/// Opens system-mix capture; with `exclude_self` the current process is excluded from the tap
/// (so a device that is both hub and sender never captures its own mix).
pub(crate) fn open_system(exclude_self: bool) -> Result<Box<dyn CaptureSource>, CaptureError> {
    let target = if exclude_self {
        TapTarget::SystemExcludingSelf
    } else {
        TapTarget::System
    };
    Ok(Box::new(TapCapture::open(target)?))
}

/// Opens capture of one process (a stereo mixdown of its audio).
pub(crate) fn open_process(pid: u32) -> Result<Box<dyn CaptureSource>, CaptureError> {
    Ok(Box::new(TapCapture::open(TapTarget::Process { pid })?))
}

/// Lists processes that are currently producing audio output (excluding this process).
pub(crate) fn list_apps() -> Result<Vec<CaptureApp>, CaptureError> {
    // Before macOS 14.2 no app could be captured anyway (and the property may not exist).
    ensure_supported()?;
    let own_pid = std::process::id();
    let objects = read_process_objects().map_err(|s| os_error("reading the process list", s))?;
    let mut apps = Vec::new();
    for object in objects {
        // SAFETY: kAudioProcessPropertyIsRunningOutput is a UInt32.
        let running =
            unsafe { read_property::<u32>(object, kAudioProcessPropertyIsRunningOutput, None, 0) };
        if !matches!(running, Ok(r) if r != 0) {
            continue;
        }
        // SAFETY: kAudioProcessPropertyPID is a pid_t (i32).
        let pid = match unsafe { read_property::<i32>(object, kAudioProcessPropertyPID, None, -1) }
        {
            Ok(pid) => match u32::try_from(pid) {
                Ok(pid) if pid != own_pid => pid,
                _ => continue,
            },
            Err(_) => continue,
        };
        let bundle_id = read_string_property(object, kAudioProcessPropertyBundleID);
        let name = app_name(pid, bundle_id.as_deref());
        apps.push(CaptureApp { pid, name });
    }
    sort_and_dedup_apps(&mut apps);
    Ok(apps)
}

// ---------------------------------------------------------------------------------------------
// Availability
// ---------------------------------------------------------------------------------------------

/// `AudioHardwareCreateProcessTap(CATapDescription *, AudioObjectID *)`.
type CreateProcessTapFn =
    unsafe extern "C-unwind" fn(*const CATapDescription, *mut AudioObjectID) -> OsStatus;
/// `AudioHardwareDestroyProcessTap(AudioObjectID)`.
type DestroyProcessTapFn = unsafe extern "C-unwind" fn(AudioObjectID) -> OsStatus;

/// The macOS 14.2+ tap functions, resolved at run time.
#[derive(Clone, Copy)]
struct TapFns {
    create: CreateProcessTapFn,
    destroy: DestroyProcessTapFn,
}

/// Looks up a CoreAudio symbol in the already-loaded images.
fn lookup_symbol(name: &CStr) -> Option<NonNull<c_void>> {
    // SAFETY: `name` is a valid NUL-terminated string; RTLD_DEFAULT searches every image loaded
    // into the process (CoreAudio is linked by `objc2-core-audio`). dlsym has no other
    // preconditions and returns NULL when the symbol does not exist.
    NonNull::new(unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) })
}

/// Resolves the tap functions once (they are missing before macOS 14.2).
fn tap_fns() -> Option<TapFns> {
    static FNS: OnceLock<Option<TapFns>> = OnceLock::new();
    *FNS.get_or_init(|| {
        let create = lookup_symbol(c"AudioHardwareCreateProcessTap")?;
        let destroy = lookup_symbol(c"AudioHardwareDestroyProcessTap")?;
        // SAFETY: both symbols are the CoreAudio functions declared in AudioHardware.h with
        // exactly these C signatures (`OSStatus (CATapDescription*, AudioObjectID*)` and
        // `OSStatus (AudioObjectID)`); a data pointer returned by dlsym for a function symbol
        // is that function's address.
        unsafe {
            Some(TapFns {
                create: mem::transmute::<*mut c_void, CreateProcessTapFn>(create.as_ptr()),
                destroy: mem::transmute::<*mut c_void, DestroyProcessTapFn>(destroy.as_ptr()),
            })
        }
    })
}

/// Checks that this macOS has Core Audio process taps.
fn ensure_supported() -> Result<TapFns, CaptureError> {
    let unsupported = || {
        CaptureError::Unsupported(format!(
            "system audio capture needs macOS {}.{} or newer (Core Audio process taps)",
            MIN_MACOS.0, MIN_MACOS.1
        ))
    };
    if !objc2::available!(macos = 14.2) || AnyClass::get(c"CATapDescription").is_none() {
        return Err(unsupported());
    }
    tap_fns().ok_or_else(unsupported)
}

// ---------------------------------------------------------------------------------------------
// The capture source
// ---------------------------------------------------------------------------------------------

/// What a [`TapCapture`] taps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TapTarget {
    /// Every process.
    System,
    /// Every process except this one.
    SystemExcludingSelf,
    /// One process.
    Process { pid: u32 },
}

impl TapTarget {
    fn describe(self) -> String {
        match self {
            TapTarget::System => "System audio (Core Audio process tap)".to_owned(),
            TapTarget::SystemExcludingSelf => {
                "System audio except this app (Core Audio process tap)".to_owned()
            }
            TapTarget::Process { pid } => {
                format!(
                    "{} (pid {pid}, Core Audio process tap)",
                    app_name(pid, None)
                )
            }
        }
    }
}

/// A Core Audio process tap feeding a [`PcmSink`] through a private aggregate device.
struct TapCapture {
    target: TapTarget,
    format: AudioFormat,
    /// Field order matters for drops: the IOProc goes before the device and tap it uses.
    running: Option<IoProc>,
    device: Option<TapDevice>,
    /// IOProc calls whose buffer layout did not match the tap format (dropped).
    layout_mismatches: Arc<AtomicU64>,
}

impl TapCapture {
    fn open(target: TapTarget) -> Result<Self, CaptureError> {
        let fns = ensure_supported()?;
        let device = TapDevice::create(fns, target)?;
        tracing::info!(
            target_desc = %target.describe(),
            sample_rate = device.format.sample_rate,
            channels = device.format.channels,
            "opened Core Audio process tap"
        );
        Ok(Self {
            target,
            format: device.format,
            running: None,
            device: Some(device),
            layout_mismatches: Arc::new(AtomicU64::new(0)),
        })
    }
}

impl CaptureSource for TapCapture {
    fn describe(&self) -> String {
        self.target.describe()
    }

    fn format(&self) -> AudioFormat {
        self.format
    }

    fn start(&mut self, sink: PcmSink) -> Result<(), CaptureError> {
        if self.running.is_some() {
            return Err(CaptureError::AlreadyRunning);
        }
        // After `stop` the OS objects are gone; build them again for a restart.
        let device = match self.device.take() {
            Some(device) => device,
            None => {
                let device = TapDevice::create(ensure_supported()?, self.target)?;
                if device.format != self.format {
                    return Err(CaptureError::Format(format!(
                        "the tap format changed from {:?} to {:?}; open the capture again",
                        self.format, device.format
                    )));
                }
                device
            }
        };
        let channels = usize::from(self.format.channels);
        let context = IoContext {
            sink,
            channels,
            scratch: vec![0.0; SCRATCH_FRAMES * channels].into_boxed_slice(),
            layout_mismatches: Arc::clone(&self.layout_mismatches),
            health: device.watch.health,
        };
        let started = IoProc::start(device.aggregate.id, context);
        self.device = Some(device);
        self.running = Some(started?);
        tracing::info!(target_desc = %self.target.describe(), "Core Audio tap capture started");
        Ok(())
    }

    fn stop(&mut self) {
        let was_running = self.running.is_some();
        // Order: AudioDeviceStop + AudioDeviceDestroyIOProcID (IoProc::drop), then
        // AudioHardwareDestroyAggregateDevice and AudioHardwareDestroyProcessTap (TapDevice).
        self.running = None;
        self.device = None;
        if was_running {
            let mismatches = self.layout_mismatches.swap(0, Ordering::Relaxed);
            if mismatches > 0 {
                tracing::warn!(
                    mismatches,
                    "Core Audio tap delivered buffers with an unexpected layout (dropped)"
                );
            }
            tracing::info!(target_desc = %self.target.describe(), "Core Audio tap capture stopped");
        }
    }

    fn error(&self) -> Option<String> {
        let reason = self.device.as_ref()?.watch.health.failure()?;
        Some(format!("{reason}; open the capture again"))
    }
}

impl Drop for TapCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------------------------
// OS objects (RAII)
// ---------------------------------------------------------------------------------------------

/// A process tap object; destroyed on drop.
struct ProcessTap {
    id: AudioObjectID,
    destroy: DestroyProcessTapFn,
}

impl Drop for ProcessTap {
    fn drop(&mut self) {
        // SAFETY: `id` is a tap this value created and has not destroyed yet; `destroy` is
        // AudioHardwareDestroyProcessTap, resolved by `tap_fns`.
        let status = unsafe { (self.destroy)(self.id) };
        if status != NO_ERR {
            tracing::warn!(status = %status_string(status), "AudioHardwareDestroyProcessTap failed");
        }
    }
}

/// A private aggregate device; destroyed on drop.
struct AggregateDevice {
    id: AudioObjectID,
}

impl Drop for AggregateDevice {
    fn drop(&mut self) {
        // SAFETY: `id` is an aggregate device this value created and has not destroyed yet.
        let status = unsafe { AudioHardwareDestroyAggregateDevice(self.id) };
        if status != NO_ERR {
            tracing::warn!(
                status = %status_string(status),
                "AudioHardwareDestroyAggregateDevice failed"
            );
        }
    }
}

/// A tap plus the private aggregate device that exposes it as an input stream.
struct TapDevice {
    /// Declared first: its listeners are removed before the objects they watch go away.
    watch: HealthWatch,
    /// Dropped (destroyed) before the tap it contains.
    aggregate: AggregateDevice,
    _tap: ProcessTap,
    format: AudioFormat,
}

impl TapDevice {
    fn create(fns: TapFns, target: TapTarget) -> Result<Self, CaptureError> {
        // Callers may be plain Rust threads without an autorelease pool; drain the Foundation
        // objects created here (tap description, UUIDs, strings) right away.
        objc2::rc::autoreleasepool(|_| Self::create_in_pool(fns, target))
    }

    fn create_in_pool(fns: TapFns, target: TapTarget) -> Result<Self, CaptureError> {
        let description = tap_description(target)?;
        // The UUID the aggregate device uses to find the tap.
        // SAFETY: plain property getter on a live CATapDescription.
        let tap_uuid = unsafe { description.UUID() }.UUIDString().to_string();

        let mut tap_id: AudioObjectID = kAudioObjectUnknown;
        // SAFETY: `fns.create` is AudioHardwareCreateProcessTap; the description is a valid,
        // retained CATapDescription for the duration of the call and `tap_id` is a valid
        // out-pointer.
        let status = unsafe { (fns.create)(Retained::as_ptr(&description), &mut tap_id) };
        if status != NO_ERR || tap_id == kAudioObjectUnknown {
            return Err(tap_create_error(status));
        }
        let tap = ProcessTap {
            id: tap_id,
            destroy: fns.destroy,
        };

        // SAFETY: kAudioTapPropertyFormat is an AudioStreamBasicDescription (plain C struct).
        let tap_asbd = unsafe {
            read_property::<AudioStreamBasicDescription>(
                tap.id,
                kAudioTapPropertyFormat,
                None,
                EMPTY_ASBD,
            )
        }
        .map_err(|s| os_error("reading the tap format", s))?;
        // Only for diagnostics: the IOProc receives the aggregate device's format (below).
        let tap_format = format_from_asbd(&tap_asbd)?;

        let aggregate_uid = format!("org.headphone-for-all.tap.{}", NSUUID::UUID().UUIDString());
        let dict = aggregate_description(&aggregate_uid, &tap_uuid);
        let mut aggregate_id: AudioObjectID = kAudioObjectUnknown;
        // SAFETY: `dict` is a valid CFDictionary for the duration of the call and
        // `aggregate_id` is a valid out-pointer.
        let status = unsafe {
            AudioHardwareCreateAggregateDevice(dict.as_opaque(), NonNull::from(&mut aggregate_id))
        };
        if status != NO_ERR || aggregate_id == kAudioObjectUnknown {
            // `tap` is destroyed by its Drop.
            return Err(os_error("creating the aggregate device", status));
        }
        let aggregate = AggregateDevice { id: aggregate_id };

        // The IOProc runs on the aggregate device and receives its input stream, in the format
        // the aggregate chose (drift compensation resamples the tap to the aggregate's clock).
        // That, not the tap format, is what `format()` must report. On error both objects are
        // destroyed by their Drop (aggregate first).
        let format = wait_for_input_format(aggregate.id)?;
        if format == tap_format {
            tracing::debug!(?format, "aggregate device input format matches the tap");
        } else {
            tracing::info!(
                ?tap_format,
                aggregate_format = ?format,
                "aggregate device input format differs from the tap format; using the aggregate's"
            );
        }
        let watch = HealthWatch::install(aggregate.id, format);
        Ok(Self {
            watch,
            aggregate,
            _tap: tap,
            format,
        })
    }
}

/// Why a tap device stopped being usable, as stored in [`DeviceHealth::failure`].
const HEALTH_OK: u32 = 0;
const HEALTH_DEVICE_DIED: u32 = 1;
const HEALTH_SERVER_RESTARTED: u32 = 2;
const HEALTH_FORMAT_CHANGED: u32 = 3;

/// What the property listeners of one tap device compare against and report into.
struct DeviceHealth {
    /// `HEALTH_OK` until the first failure (never reset: the device is rebuilt instead).
    failure: AtomicU32,
    /// The format the source reported (the input stream's virtual format at open).
    format: AudioFormat,
    /// The aggregate device's nominal sample rate at open (`None` if it could not be read).
    nominal_rate: Option<f64>,
}

impl DeviceHealth {
    fn fail(&self, reason: u32) {
        // Keep the first reason.
        let _ =
            self.failure
                .compare_exchange(HEALTH_OK, reason, Ordering::AcqRel, Ordering::Acquire);
    }

    fn failed(&self) -> bool {
        self.failure.load(Ordering::Acquire) != HEALTH_OK
    }

    /// Why the device is unusable, if it is.
    fn failure(&self) -> Option<&'static str> {
        match self.failure.load(Ordering::Acquire) {
            HEALTH_OK => None,
            HEALTH_DEVICE_DIED => Some("the Core Audio tap device went away"),
            HEALTH_SERVER_RESTARTED => Some("the Core Audio server restarted"),
            _ => Some(
                "the output device changed the captured sample rate or format (e.g. another \
                 default output device)",
            ),
        }
    }

    /// Checks one changed property of `object` against what the source reported.
    fn on_changed(&self, object: AudioObjectID, selector: AudioObjectPropertySelector) {
        match selector {
            s if s == kAudioHardwarePropertyServiceRestarted => self.fail(HEALTH_SERVER_RESTARTED),
            s if s == kAudioDevicePropertyDeviceIsAlive => {
                // SAFETY: kAudioDevicePropertyDeviceIsAlive is a UInt32.
                let alive = unsafe { read_property::<u32>(object, selector, None, 1) };
                if !matches!(alive, Ok(alive) if alive != 0) {
                    self.fail(HEALTH_DEVICE_DIED);
                }
            }
            s if s == kAudioDevicePropertyNominalSampleRate => {
                // SAFETY: kAudioDevicePropertyNominalSampleRate is a Float64.
                let rate = unsafe { read_property::<f64>(object, selector, None, 0.0) };
                if let (Ok(rate), Some(expected)) = (rate, self.nominal_rate) {
                    if (rate - expected).abs() > 0.5 {
                        self.fail(HEALTH_FORMAT_CHANGED);
                    }
                }
            }
            s if s == kAudioStreamPropertyVirtualFormat => {
                // SAFETY: kAudioStreamPropertyVirtualFormat is an AudioStreamBasicDescription.
                let asbd = unsafe {
                    read_property::<AudioStreamBasicDescription>(object, selector, None, EMPTY_ASBD)
                };
                if let Ok(asbd) = asbd {
                    if format_from_asbd(&asbd).ok() != Some(self.format) {
                        self.fail(HEALTH_FORMAT_CHANGED);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Property listeners that watch a tap device ([`DeviceHealth`]); removed on drop.
struct HealthWatch {
    /// Leaked on purpose (a few bytes per opened device): Core Audio does not promise that no
    /// listener call is still running once `AudioObjectRemovePropertyListener` returned, so the
    /// listeners' data must never be freed.
    health: &'static DeviceHealth,
    registered: Vec<(AudioObjectID, AudioObjectPropertyAddress)>,
}

impl HealthWatch {
    /// Watches `aggregate` (and its input stream, and the audio server) against `format`.
    /// Listeners that cannot be installed are logged and skipped (the capture still works,
    /// only that change goes unnoticed).
    fn install(aggregate: AudioObjectID, format: AudioFormat) -> Self {
        // SAFETY: kAudioDevicePropertyNominalSampleRate is a Float64.
        let nominal_rate = unsafe {
            read_property::<f64>(aggregate, kAudioDevicePropertyNominalSampleRate, None, 0.0)
        }
        .ok()
        .filter(|r| r.is_finite() && *r > 0.0);
        let health: &'static DeviceHealth = Box::leak(Box::new(DeviceHealth {
            failure: AtomicU32::new(HEALTH_OK),
            format,
            nominal_rate,
        }));
        let mut watch = Self {
            health,
            registered: Vec::new(),
        };
        watch.add(aggregate, kAudioDevicePropertyDeviceIsAlive);
        watch.add(aggregate, kAudioDevicePropertyNominalSampleRate);
        let input = property_address(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput);
        let streams = read_object_list(aggregate, input).unwrap_or_default();
        if let Some(&stream) = streams.first() {
            watch.add(stream, kAudioStreamPropertyVirtualFormat);
        }
        watch.add(SYSTEM_OBJECT, kAudioHardwarePropertyServiceRestarted);
        watch
    }

    fn add(&mut self, object: AudioObjectID, selector: AudioObjectPropertySelector) {
        let address = global_address(selector);
        // SAFETY: `address` is valid for the call; `health_listener` matches
        // AudioObjectPropertyListenerProc and its client data (`self.health`) is never freed.
        let status = unsafe {
            AudioObjectAddPropertyListener(
                object,
                NonNull::from(&address),
                Some(health_listener),
                ptr::from_ref(self.health).cast_mut().cast(),
            )
        };
        if status == NO_ERR {
            self.registered.push((object, address));
        } else {
            tracing::debug!(
                object,
                selector,
                status = %status_string(status),
                "cannot watch a Core Audio property"
            );
        }
    }
}

impl Drop for HealthWatch {
    fn drop(&mut self) {
        for (object, address) in self.registered.drain(..) {
            // SAFETY: exactly the (object, address, listener, client data) registered in `add`.
            let status = unsafe {
                AudioObjectRemovePropertyListener(
                    object,
                    NonNull::from(&address),
                    Some(health_listener),
                    ptr::from_ref(self.health).cast_mut().cast(),
                )
            };
            if status != NO_ERR {
                tracing::debug!(status = %status_string(status), "removing a Core Audio listener failed");
            }
        }
    }
}

/// Property listener of [`HealthWatch`] (runs on a Core Audio notification thread, not the
/// I/O thread).
unsafe extern "C-unwind" fn health_listener(
    object: AudioObjectID,
    count: u32,
    addresses: NonNull<AudioObjectPropertyAddress>,
    client_data: *mut c_void,
) -> OsStatus {
    if client_data.is_null() {
        return NO_ERR;
    }
    // SAFETY: `client_data` is the leaked `&'static DeviceHealth` registered in
    // `HealthWatch::add`.
    let health = unsafe { &*client_data.cast::<DeviceHealth>() };
    // SAFETY: Core Audio passes `count` valid addresses for the duration of the call.
    let addresses = unsafe { std::slice::from_raw_parts(addresses.as_ptr(), count as usize) };
    for address in addresses {
        health.on_changed(object, address.mSelector);
    }
    NO_ERR
}

/// Reads the aggregate device's input stream format (what the IOProc receives), retrying while
/// the freshly created device has no usable input stream yet.
fn wait_for_input_format(device: AudioObjectID) -> Result<AudioFormat, CaptureError> {
    let mut last_error = CaptureError::Backend(
        "Core Audio: the aggregate device never exposed the tap's input stream".to_owned(),
    );
    for attempt in 0..DEVICE_READY_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(DEVICE_READY_DELAY);
        }
        match read_input_stream_format(device) {
            Ok(Some(asbd)) => match format_from_asbd(&asbd) {
                Ok(format) => return Ok(format),
                // E.g. a zero sample rate while the device is still coming up.
                Err(e) => last_error = e,
            },
            Ok(None) => {}
            Err(status) => {
                last_error = os_error("reading the aggregate device's input format", status);
            }
        }
    }
    Err(last_error)
}

/// The virtual format of the device's first input stream, `Ok(None)` if it has no input stream.
fn read_input_stream_format(
    device: AudioObjectID,
) -> Result<Option<AudioStreamBasicDescription>, OsStatus> {
    let streams = read_object_list(
        device,
        property_address(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput),
    )?;
    let Some(&stream) = streams.first() else {
        return Ok(None);
    };
    if streams.len() > 1 {
        tracing::debug!(
            streams = streams.len(),
            "aggregate device has several input streams; using the first"
        );
    }
    // SAFETY: kAudioStreamPropertyVirtualFormat is an AudioStreamBasicDescription (plain C
    // struct), read from a stream object in the global scope.
    let asbd = unsafe {
        read_property::<AudioStreamBasicDescription>(
            stream,
            kAudioStreamPropertyVirtualFormat,
            None,
            EMPTY_ASBD,
        )
    }?;
    Ok(Some(asbd))
}

/// Builds the `CATapDescription` for `target`.
fn tap_description(target: TapTarget) -> Result<Retained<CATapDescription>, CaptureError> {
    let description = match target {
        TapTarget::System | TapTarget::SystemExcludingSelf => {
            let mut excluded = Vec::new();
            match (own_process_object(), target) {
                // This app's own playback (a hub's mix) is never wanted in a system capture:
                // it would be sent back to a hub, and `MUTE_BEHAVIOR` would silence it here.
                // So the plain system tap leaves us out too whenever Core Audio knows us.
                (Ok(Some(object)), _) => excluded.push(NSNumber::numberWithUnsignedInt(object)),
                // Never fall back to a tap that includes us: once this device also plays the
                // hub mix, it would be captured again (a feedback loop).
                (Ok(None), TapTarget::SystemExcludingSelf) => {
                    return Err(CaptureError::Backend(
                        "this process is not registered with Core Audio yet, so it cannot be \
                         excluded from the system tap"
                            .to_owned(),
                    ))
                }
                (Err(status), TapTarget::SystemExcludingSelf) => {
                    return Err(os_error("looking up this process", status))
                }
                // Not registered yet means we play nothing yet: the plain tap is fine.
                (Ok(None) | Err(_), _) => {}
            }
            let excluded = NSArray::from_retained_slice(&excluded);
            // SAFETY: `alloc` returns a fresh CATapDescription allocation (class availability
            // was checked by `ensure_supported`); the initializer takes an NSArray<NSNumber> of
            // process object ids.
            unsafe {
                CATapDescription::initStereoGlobalTapButExcludeProcesses(
                    CATapDescription::alloc(),
                    &excluded,
                )
            }
        }
        TapTarget::Process { pid } => {
            let object = match translate_pid(pid) {
                Ok(Some(object)) => object,
                Ok(None) => {
                    return Err(CaptureError::NotFound(format!(
                        "process {pid} is not known to Core Audio (is it running and has it \
                         played audio?)"
                    )))
                }
                Err(status) => return Err(os_error("looking up the process", status)),
            };
            let included = NSArray::from_retained_slice(&[NSNumber::numberWithUnsignedInt(object)]);
            // SAFETY: as above, with the process objects to include.
            unsafe {
                CATapDescription::initStereoMixdownOfProcesses(CATapDescription::alloc(), &included)
            }
        }
    };
    // SAFETY: plain property setters on a live, uniquely owned CATapDescription; the arguments
    // are valid objects/enum values.
    unsafe {
        description.setName(&NSString::from_str(DEVICE_NAME));
        description.setUUID(&NSUUID::UUID());
        description.setPrivate(true);
        description.setMuteBehavior(MUTE_BEHAVIOR);
    }
    Ok(description)
}

/// Builds the aggregate device description (AudioCap's dictionary without the output
/// sub-device): name, UID, private, not stacked, tap auto-start and a one-entry tap list.
fn aggregate_description(
    aggregate_uid: &str,
    tap_uuid: &str,
) -> CFRetained<CFDictionary<CFString, CFType>> {
    let key = |k: &CStr| CFString::from_str(&k.to_string_lossy());
    let one = CFNumber::new_i32(1);
    let zero = CFNumber::new_i32(0);

    let tap_entry = CFDictionary::<CFString, CFType>::from_slices(
        &[
            &*key(kAudioSubTapUIDKey),
            &*key(kAudioSubTapDriftCompensationKey),
        ],
        &[&CFString::from_str(tap_uuid), &one],
    );
    let tap_list = CFArray::from_objects(&[&*tap_entry]);

    let name = CFString::from_str(DEVICE_NAME);
    let uid = CFString::from_str(aggregate_uid);
    let keys = [
        key(kAudioAggregateDeviceNameKey),
        key(kAudioAggregateDeviceUIDKey),
        key(kAudioAggregateDeviceIsPrivateKey),
        key(kAudioAggregateDeviceIsStackedKey),
        key(kAudioAggregateDeviceTapAutoStartKey),
        key(kAudioAggregateDeviceTapListKey),
    ];
    let values: [&CFType; 6] = [&name, &uid, &one, &zero, &one, &tap_list];
    let keys: Vec<&CFString> = keys.iter().map(|k| &**k).collect();
    CFDictionary::from_slices(&keys, &values)
}

// ---------------------------------------------------------------------------------------------
// IOProc (real-time)
// ---------------------------------------------------------------------------------------------

/// State owned by the IOProc while it runs. Only the Core Audio I/O thread touches it between
/// `AudioDeviceStart` and `AudioDeviceDestroyIOProcID`.
struct IoContext {
    sink: PcmSink,
    channels: usize,
    /// Interleaving buffer for non-interleaved input (`SCRATCH_FRAMES * channels` samples).
    scratch: Box<[f32]>,
    layout_mismatches: Arc<AtomicU64>,
    /// Once the device failed (format change...), nothing is pushed any more.
    health: &'static DeviceHealth,
}

/// Our IOProc with raw pointers: Core Audio may pass NULL for buffer lists it does not use
/// (e.g. the output list of an input-only device), which a `NonNull` parameter cannot represent.
type RawIoProc = unsafe extern "C-unwind" fn(
    AudioObjectID,
    *const AudioTimeStamp,
    *const AudioBufferList,
    *const AudioTimeStamp,
    *mut AudioBufferList,
    *const AudioTimeStamp,
    *mut c_void,
) -> OsStatus;

/// The non-`Option` function type inside [`AudioDeviceIOProc`].
type BoundIoProc = unsafe extern "C-unwind" fn(
    AudioObjectID,
    NonNull<AudioTimeStamp>,
    NonNull<AudioBufferList>,
    NonNull<AudioTimeStamp>,
    NonNull<AudioBufferList>,
    NonNull<AudioTimeStamp>,
    *mut c_void,
) -> OsStatus;

/// The IOProc: copies the tap's input buffers into the ring. Never allocates, locks, logs or
/// makes syscalls, and cannot panic.
unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: *const AudioTimeStamp,
    input: *const AudioBufferList,
    _input_time: *const AudioTimeStamp,
    _output: *mut AudioBufferList,
    _output_time: *const AudioTimeStamp,
    client_data: *mut c_void,
) -> OsStatus {
    if client_data.is_null() || input.is_null() {
        return NO_ERR;
    }
    // SAFETY: `client_data` is the `Box<IoContext>` pointer registered in `IoProc::start`. It
    // stays valid until the IOProc is destroyed, and Core Audio calls one IOProc serially, so
    // this is the only live reference.
    let context = unsafe { &mut *client_data.cast::<IoContext>() };
    // SAFETY: `input` points to a valid AudioBufferList whose `mBuffers` is a C flexible array
    // of `mNumberBuffers` entries, valid for the duration of this call.
    let buffers = unsafe {
        let count = (*input).mNumberBuffers as usize;
        let first = ptr::addr_of!((*input).mBuffers).cast::<AudioBuffer>();
        std::slice::from_raw_parts(first, count)
    };
    let IoContext {
        sink,
        channels,
        scratch,
        layout_mismatches,
        health,
    } = context;
    if health.failed() {
        // E.g. the rate changed: pushing would play at the wrong pitch.
        return NO_ERR;
    }
    // SAFETY: every buffer's `mData` points to `mDataByteSize` readable bytes for the duration
    // of this call (Core Audio IOProc contract).
    let delivered = unsafe {
        deliver(buffers, *channels, scratch, |samples| {
            sink.push(samples);
        })
    };
    if !delivered {
        layout_mismatches.fetch_add(1, Ordering::Relaxed);
    }
    NO_ERR
}

/// A registered and started IOProc; stopped and destroyed on drop.
struct IoProc {
    device: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    context: NonNull<IoContext>,
}

// SAFETY: `context` is a uniquely owned heap allocation that only the Core Audio I/O thread
// dereferences while the IOProc is registered; the owner frees it only after
// AudioDeviceDestroyIOProcID returned. `IoContext` itself is `Send`.
unsafe impl Send for IoProc {}

impl IoProc {
    fn start(device: AudioObjectID, context: IoContext) -> Result<Self, CaptureError> {
        let context = NonNull::from(Box::leak(Box::new(context)));
        // SAFETY: `RawIoProc` and `BoundIoProc` differ only in `*const T`/`*mut T` versus
        // `NonNull<T>` parameters, which are ABI-compatible; our function accepts NULL.
        let callback: AudioDeviceIOProc =
            Some(unsafe { mem::transmute::<RawIoProc, BoundIoProc>(io_proc) });
        let mut proc_id: AudioDeviceIOProcID = None;
        // SAFETY: `device` is our live aggregate device, `context` outlives the registration
        // (freed only after AudioDeviceDestroyIOProcID) and `proc_id` is a valid out-pointer.
        let status = unsafe {
            AudioDeviceCreateIOProcID(
                device,
                callback,
                context.as_ptr().cast(),
                NonNull::from(&mut proc_id),
            )
        };
        if status != NO_ERR || proc_id.is_none() {
            // SAFETY: no IOProc was registered, so nothing else references `context`.
            drop(unsafe { Box::from_raw(context.as_ptr()) });
            return Err(os_error("registering the audio callback", status));
        }
        let io = IoProc {
            device,
            proc_id,
            context,
        };
        // SAFETY: `proc_id` was just created on `device`.
        let status = unsafe { AudioDeviceStart(device, io.proc_id) };
        if status != NO_ERR {
            // `io`'s Drop destroys the IOProc and frees the context.
            return Err(os_error("starting the tap device", status));
        }
        Ok(io)
    }
}

impl Drop for IoProc {
    fn drop(&mut self) {
        // SAFETY: `proc_id` is registered on `device`; stopping an IOProc that did not start
        // is harmless.
        let stop = unsafe { AudioDeviceStop(self.device, self.proc_id) };
        if stop != NO_ERR {
            tracing::warn!(status = %status_string(stop), "AudioDeviceStop failed");
        }
        // SAFETY: as above; after this returns Core Audio no longer calls the IOProc.
        let destroy = unsafe { AudioDeviceDestroyIOProcID(self.device, self.proc_id) };
        if destroy == NO_ERR {
            // SAFETY: the IOProc is gone, so this is the only reference to the context.
            drop(unsafe { Box::from_raw(self.context.as_ptr()) });
        } else {
            // The callback might still be registered: leak the context rather than risk a
            // use-after-free (the aggregate device is destroyed next anyway).
            tracing::warn!(
                status = %status_string(destroy),
                "AudioDeviceDestroyIOProcID failed; leaking the capture context"
            );
        }
    }
}

/// Views one `AudioBuffer` as `f32` samples. `None` for NULL or misaligned data.
///
/// # Safety
/// `buffer.mData` must be NULL or point to `mDataByteSize` bytes that stay readable while the
/// returned slice (which borrows `buffer`) lives.
unsafe fn buffer_samples(buffer: &AudioBuffer) -> Option<&[f32]> {
    let data = buffer.mData.cast::<f32>().cast_const();
    if data.is_null() || !data.is_aligned() {
        return None;
    }
    let len = buffer.mDataByteSize as usize / size_of::<f32>();
    // SAFETY: non-null, aligned, and `len * 4 <= mDataByteSize` readable bytes (caller contract).
    Some(unsafe { std::slice::from_raw_parts(data, len) })
}

/// Pushes the samples of one IOProc call to `push` as interleaved `f32` (real-time safe).
///
/// Accepts either one interleaved buffer carrying all `channels`, or (non-interleaved)
/// `channels` mono buffers, which are interleaved through `scratch` in chunks. Returns `false`
/// (nothing pushed) when the buffers match neither layout.
///
/// # Safety
/// Every buffer's `mData` must be NULL or point to `mDataByteSize` readable bytes.
unsafe fn deliver(
    buffers: &[AudioBuffer],
    channels: usize,
    scratch: &mut [f32],
    mut push: impl FnMut(&[f32]),
) -> bool {
    if channels == 0 {
        return false;
    }
    let Some(first) = buffers.first() else {
        return false;
    };
    if first.mNumberChannels as usize == channels {
        // Interleaved (or mono): pass whole frames straight through.
        // SAFETY: caller contract.
        let Some(samples) = (unsafe { buffer_samples(first) }) else {
            return false;
        };
        let whole = samples.len() - samples.len() % channels;
        if let Some(frames) = samples.get(..whole) {
            if !frames.is_empty() {
                push(frames);
            }
        }
        return true;
    }
    if channels > MAX_PLANAR_CHANNELS
        || buffers.len() < channels
        || buffers
            .iter()
            .take(channels)
            .any(|b| b.mNumberChannels != 1)
    {
        return false;
    }
    let mut planes: [&[f32]; MAX_PLANAR_CHANNELS] = [&[]; MAX_PLANAR_CHANNELS];
    for (plane, buffer) in planes.iter_mut().zip(buffers).take(channels) {
        // SAFETY: caller contract.
        match unsafe { buffer_samples(buffer) } {
            Some(samples) => *plane = samples,
            None => return false,
        }
    }
    let planes = &planes[..channels];
    let frames = planes.iter().map(|p| p.len()).min().unwrap_or(0);
    let chunk_frames = scratch.len() / channels;
    if chunk_frames == 0 {
        return false;
    }
    let mut offset = 0;
    while offset < frames {
        let n = chunk_frames.min(frames - offset);
        let Some(out) = scratch.get_mut(..n * channels) else {
            return false;
        };
        interleave(planes, offset, out);
        push(out);
        offset += n;
    }
    true
}

/// Interleaves `out.len() / planes.len()` frames starting at frame `offset` of each plane.
/// Missing samples (a plane shorter than requested) become silence.
fn interleave(planes: &[&[f32]], offset: usize, out: &mut [f32]) {
    if planes.is_empty() {
        return;
    }
    for (frame, slots) in out.chunks_exact_mut(planes.len()).enumerate() {
        for (slot, plane) in slots.iter_mut().zip(planes) {
            *slot = plane.get(offset + frame).copied().unwrap_or(0.0);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Property helpers
// ---------------------------------------------------------------------------------------------

fn property_address(
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn global_address(selector: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
    property_address(selector, kAudioObjectPropertyScopeGlobal)
}

/// The system object (`kAudioObjectSystemObject`).
const SYSTEM_OBJECT: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;

/// Reads a fixed-size global property, starting from `init`.
///
/// # Safety
/// `T` must be the plain-old-data C type Core Audio stores for `selector` (any bit pattern the
/// HAL writes must be a valid `T`), and `qualifier` must be what the selector expects.
unsafe fn read_property<T: Copy>(
    object: AudioObjectID,
    selector: AudioObjectPropertySelector,
    qualifier: Option<&[u8]>,
    init: T,
) -> Result<T, OsStatus> {
    let address = global_address(selector);
    let (q_size, q_data) = match qualifier {
        Some(q) => (q.len() as u32, q.as_ptr().cast::<c_void>()),
        None => (0, ptr::null()),
    };
    let mut value = init;
    let mut size = size_of::<T>() as u32;
    // SAFETY: the address, qualifier and size pointers are valid for the call; `value` has room
    // for `size` bytes and the HAL writes at most `size` bytes of a `T` (caller contract).
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            q_size,
            q_data,
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    if status == NO_ERR {
        Ok(value)
    } else {
        Err(status)
    }
}

/// Reads a CFString property (the HAL returns a +1 reference that we release).
fn read_string_property(
    object: AudioObjectID,
    selector: AudioObjectPropertySelector,
) -> Option<String> {
    // SAFETY: CFString-valued properties store a `CFStringRef` (a pointer, NULL allowed).
    let raw =
        unsafe { read_property::<*const CFString>(object, selector, None, ptr::null()) }.ok()?;
    let raw = NonNull::new(raw.cast_mut())?;
    // SAFETY: Core Audio hands out CF objects from property getters with a +1 retain count
    // ("the caller is responsible for releasing"); `CFRetained` takes over that reference.
    let string = unsafe { CFRetained::from_raw(raw) };
    let string = string.to_string();
    (!string.is_empty()).then_some(string)
}

/// `kAudioHardwarePropertyTranslatePIDToProcessObject`: `Ok(None)` if the audio server does not
/// know the process (including pids that cannot exist because they do not fit in a `pid_t`).
fn translate_pid(pid: u32) -> Result<Option<AudioObjectID>, OsStatus> {
    let Ok(pid) = i32::try_from(pid) else {
        return Ok(None);
    };
    let qualifier = pid.to_ne_bytes();
    // SAFETY: the property is an AudioObjectID (u32) qualified by a pid_t (i32).
    let object = unsafe {
        read_property::<AudioObjectID>(
            SYSTEM_OBJECT,
            kAudioHardwarePropertyTranslatePIDToProcessObject,
            Some(&qualifier),
            kAudioObjectUnknown,
        )
    }?;
    Ok((object != kAudioObjectUnknown).then_some(object))
}

/// This process's audio object. The HAL registers a process when it first talks to the audio
/// server, so while the lookup finds nothing, touch the process list and look again (a few
/// times, briefly; this never runs on a real-time thread).
fn own_process_object() -> Result<Option<AudioObjectID>, OsStatus> {
    let pid = std::process::id();
    for attempt in 0..OWN_PROCESS_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(OWN_PROCESS_DELAY);
        }
        if let Some(object) = translate_pid(pid)? {
            return Ok(Some(object));
        }
        read_process_objects()?;
    }
    translate_pid(pid)
}

/// `kAudioHardwarePropertyProcessObjectList`.
fn read_process_objects() -> Result<Vec<AudioObjectID>, OsStatus> {
    read_object_list(
        SYSTEM_OBJECT,
        global_address(kAudioHardwarePropertyProcessObjectList),
    )
}

/// Reads a variable-length `AudioObjectID` array property (unknown ids removed).
fn read_object_list(
    object: AudioObjectID,
    address: AudioObjectPropertyAddress,
) -> Result<Vec<AudioObjectID>, OsStatus> {
    let mut size: u32 = 0;
    // SAFETY: valid address and out-pointer; no qualifier.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            object,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != NO_ERR {
        return Err(status);
    }
    let mut objects: Vec<AudioObjectID> =
        vec![kAudioObjectUnknown; size as usize / size_of::<AudioObjectID>()];
    if objects.is_empty() {
        return Ok(objects);
    }
    let mut size = (objects.len() * size_of::<AudioObjectID>()) as u32;
    // SAFETY: `objects` has room for `size` bytes of AudioObjectIDs; the HAL writes at most
    // `size` bytes and reports how many it wrote.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(objects.as_mut_ptr().cast::<c_void>()).unwrap_or(NonNull::dangling()),
        )
    };
    if status != NO_ERR {
        return Err(status);
    }
    objects.truncate(size as usize / size_of::<AudioObjectID>());
    objects.retain(|&o| o != kAudioObjectUnknown);
    Ok(objects)
}

/// The name shown for a process: its app bundle's name, else a name from its bundle id, else
/// its process name, else `pid <n>`.
fn app_name(pid: u32, bundle_id: Option<&str>) -> String {
    if let Some(name) = executable_path(pid).as_deref().and_then(app_bundle_name) {
        return name.to_owned();
    }
    if let Some(name) = bundle_id_name(bundle_id) {
        return name.to_owned();
    }
    process_name(pid).unwrap_or_else(|| format!("pid {pid}"))
}

/// The process's executable path (`proc_pidpath`), if the process exists.
fn executable_path(pid: u32) -> Option<String> {
    let pid = i32::try_from(pid).ok()?;
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buf` is writable for its full length; proc_pidpath writes at most that many
    // bytes and returns the length written (<= 0 on failure).
    let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let len = usize::try_from(len)
        .ok()
        .filter(|&l| l > 0 && l <= buf.len())?;
    let path = String::from_utf8_lossy(buf.get(..len)?).into_owned();
    (!path.is_empty()).then_some(path)
}

/// The short process name (`proc_name`), if the process exists.
fn process_name(pid: u32) -> Option<String> {
    let pid = i32::try_from(pid).ok()?;
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is writable for its full length; proc_name writes at most that many bytes
    // and returns the length written (<= 0 on failure).
    let len = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let len = usize::try_from(len)
        .ok()
        .filter(|&l| l > 0 && l <= buf.len())?;
    let name = String::from_utf8_lossy(buf.get(..len)?).trim().to_owned();
    (!name.is_empty()).then_some(name)
}

// ---------------------------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------------------------

/// Validates the tap's stream format: native-endian linear PCM float32.
fn format_from_asbd(asbd: &AudioStreamBasicDescription) -> Result<AudioFormat, CaptureError> {
    let bad = |why: &str| CaptureError::Format(format!("Core Audio tap format {why}: {asbd:?}"));
    if asbd.mFormatID != kAudioFormatLinearPCM {
        return Err(bad("is not linear PCM"));
    }
    if asbd.mFormatFlags & kAudioFormatFlagIsFloat == 0 || asbd.mBitsPerChannel != 32 {
        return Err(bad("is not 32-bit float"));
    }
    if cfg!(target_endian = "little") == (asbd.mFormatFlags & kAudioFormatFlagIsBigEndian != 0) {
        return Err(bad("is not native-endian"));
    }
    let channels = u16::try_from(asbd.mChannelsPerFrame)
        .ok()
        .filter(|&c| c > 0)
        .ok_or_else(|| bad("has no channels"))?;
    let rate = asbd.mSampleRate;
    if !(rate.is_finite() && rate >= 1.0 && rate <= f64::from(u32::MAX)) {
        return Err(bad("has an invalid sample rate"));
    }
    // Record the layout for diagnostics; `deliver` accepts either layout at run time.
    tracing::debug!(
        interleaved = asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved == 0,
        "tap stream layout"
    );
    Ok(AudioFormat::new(rate.round() as u32, channels))
}

/// The name of the outermost app bundle in an executable path: what the user knows the app
/// as, also for its helper processes (`/Applications/Google Chrome.app/Contents/Frameworks/…/
/// Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper` → `Google Chrome`,
/// `/Applications/Spotify.app/Contents/MacOS/Spotify` → `Spotify`).
fn app_bundle_name(executable: &str) -> Option<&str> {
    executable
        .split('/')
        .find_map(|component| component.strip_suffix(".app"))
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

/// Bundle-id components that do not name an app on their own (`com.spotify.client`,
/// `us.zoom.xos`, `com.google.Chrome.helper`, `com.apple.WebKit.GPU`).
const GENERIC_BUNDLE_WORDS: &[&str] = &[
    "agent", "app", "audio", "client", "daemon", "desktop", "gpu", "helper", "mac", "macos", "osx",
    "plugin", "renderer", "service", "xos", "xpc",
];

/// A name from a bundle id, for processes outside an app bundle (XPC services, daemons): the
/// last component, or the one before it when the last is generic (`com.apple.Safari` →
/// `Safari`, `com.apple.WebKit.GPU` → `WebKit`, `us.zoom.xos` → `zoom`).
fn bundle_id_name(bundle_id: Option<&str>) -> Option<&str> {
    let parts: Vec<&str> = bundle_id?
        .trim()
        .split('.')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let generic = |p: &str| GENERIC_BUNDLE_WORDS.contains(&p.to_ascii_lowercase().as_str());
    let (&last, rest) = parts.split_last()?;
    if !generic(last) {
        return Some(last);
    }
    // Skip the reverse-DNS prefix (`com`, `org`, `us`...): never a name.
    match rest {
        [_, .., before] if !generic(before) => Some(before),
        _ => Some(last),
    }
}

/// Sorts by name (case-insensitive) then pid, and drops duplicate pids.
fn sort_and_dedup_apps(apps: &mut Vec<CaptureApp>) {
    apps.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.pid.cmp(&b.pid))
    });
    let mut seen = std::collections::HashSet::new();
    apps.retain(|app| seen.insert(app.pid));
}

/// Formats an `OSStatus`, as its four-char code when printable (`'nope' (1852797029)`).
fn status_string(status: OsStatus) -> String {
    let bytes = status.to_be_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        format!("'{}' ({status})", String::from_utf8_lossy(&bytes))
    } else {
        status.to_string()
    }
}

/// Maps a failed Core Audio call to a [`CaptureError`]. `'nope'` is Core Audio's generic
/// "illegal operation" status, so here it is a [`CaptureError::Backend`]; only
/// [`tap_create_error`] reads it as a missing permission.
fn os_error(what: &str, status: OsStatus) -> CaptureError {
    map_status(what, status, false)
}

/// Maps a failed `AudioHardwareCreateProcessTap`: there `'nope'` means the "System Audio
/// Recording" permission was refused.
fn tap_create_error(status: OsStatus) -> CaptureError {
    map_status("creating the process tap", status, true)
}

/// Shared mapping: `'!hog'` (and `'nope'` when `nope_is_permission`) → `PermissionDenied`,
/// `'unop'` → `Unsupported`, anything else → `Backend`.
fn map_status(what: &str, status: OsStatus, nope_is_permission: bool) -> CaptureError {
    let status_text = status_string(status);
    if status == kAudioDevicePermissionsError
        || (nope_is_permission && status == kAudioHardwareIllegalOperationError)
    {
        CaptureError::PermissionDenied(format!(
            "{what} failed ({status_text}); allow \"System Audio Recording\" for this app in \
             System Settings > Privacy & Security"
        ))
    } else if status == kAudioHardwareUnsupportedOperationError {
        CaptureError::Unsupported(format!("{what} is not supported ({status_text})"))
    } else {
        CaptureError::Backend(format!("Core Audio: {what} failed ({status_text})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(channels: u32, data: &mut [f32]) -> AudioBuffer {
        AudioBuffer {
            mNumberChannels: channels,
            mDataByteSize: std::mem::size_of_val(data) as u32,
            mData: data.as_mut_ptr().cast(),
        }
    }

    fn asbd(rate: f64, channels: u32, flags: u32, bits: u32) -> AudioStreamBasicDescription {
        AudioStreamBasicDescription {
            mSampleRate: rate,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: flags,
            mBytesPerPacket: 4 * channels,
            mFramesPerPacket: 1,
            mBytesPerFrame: 4 * channels,
            mChannelsPerFrame: channels,
            mBitsPerChannel: bits,
            mReserved: 0,
        }
    }

    #[test]
    fn interleaved_buffer_passes_through_whole_frames() {
        let mut data = [0.1, 0.2, 0.3, 0.4, 0.5]; // 2.5 stereo frames
        let buffers = [buffer(2, &mut data)];
        let mut scratch = vec![0.0; 16];
        let mut got = Vec::new();
        // SAFETY: the buffers point into live arrays.
        let ok = unsafe { deliver(&buffers, 2, &mut scratch, |s| got.extend_from_slice(s)) };
        assert!(ok);
        assert_eq!(got, [0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn planar_buffers_are_interleaved_in_chunks() {
        let mut left: Vec<f32> = (0..10).map(|i| i as f32).collect();
        let mut right: Vec<f32> = (0..10).map(|i| -(i as f32)).collect();
        let buffers = [buffer(1, &mut left), buffer(1, &mut right)];
        // Room for 3 frames per chunk: forces 4 pushes (3 + 3 + 3 + 1 frames).
        let mut scratch = vec![0.0; 6];
        let mut got = Vec::new();
        let mut pushes = 0;
        // SAFETY: the buffers point into live vectors.
        let ok = unsafe {
            deliver(&buffers, 2, &mut scratch, |s| {
                pushes += 1;
                got.extend_from_slice(s);
            })
        };
        assert!(ok);
        assert_eq!(pushes, 4);
        let expected: Vec<f32> = (0..10).flat_map(|i| [i as f32, -(i as f32)]).collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn planar_uses_the_shortest_plane() {
        let mut left = [1.0, 2.0, 3.0];
        let mut right = [4.0, 5.0];
        let buffers = [buffer(1, &mut left), buffer(1, &mut right)];
        let mut scratch = vec![0.0; 64];
        let mut got = Vec::new();
        // SAFETY: the buffers point into live arrays.
        assert!(unsafe { deliver(&buffers, 2, &mut scratch, |s| got.extend_from_slice(s)) });
        assert_eq!(got, [1.0, 4.0, 2.0, 5.0]);
    }

    #[test]
    fn unexpected_layouts_are_rejected() {
        let mut a = [0.0f32; 8];
        let mut b = [0.0f32; 8];
        let mut scratch = vec![0.0; 16];
        let mut pushed = false;
        // A 4-channel buffer for a stereo tap.
        let four = [buffer(4, &mut a)];
        // SAFETY: the buffers point into live arrays.
        assert!(!unsafe { deliver(&four, 2, &mut scratch, |_| pushed = true) });
        // Only one mono plane for a stereo tap.
        let one = [buffer(1, &mut b)];
        // SAFETY: as above.
        assert!(!unsafe { deliver(&one, 2, &mut scratch, |_| pushed = true) });
        // No buffers, and a NULL data pointer.
        // SAFETY: as above; NULL data is allowed by the contract.
        assert!(!unsafe { deliver(&[], 2, &mut scratch, |_| pushed = true) });
        let null = [AudioBuffer {
            mNumberChannels: 2,
            mDataByteSize: 64,
            mData: ptr::null_mut(),
        }];
        // SAFETY: as above.
        assert!(!unsafe { deliver(&null, 2, &mut scratch, |_| pushed = true) });
        assert!(!pushed);
    }

    #[test]
    fn interleave_fills_missing_samples_with_silence() {
        let left = [1.0, 2.0];
        let right = [3.0];
        let mut out = [9.0; 4];
        interleave(&[&left, &right], 0, &mut out);
        assert_eq!(out, [1.0, 3.0, 2.0, 0.0]);
    }

    #[test]
    fn tap_format_is_validated() {
        let float = kAudioFormatFlagIsFloat | objc2_core_audio_types::kAudioFormatFlagIsPacked;
        assert_eq!(
            format_from_asbd(&asbd(48_000.0, 2, float, 32)),
            Ok(AudioFormat::new(48_000, 2))
        );
        assert_eq!(
            format_from_asbd(&asbd(
                44_100.0,
                2,
                float | kAudioFormatFlagIsNonInterleaved,
                32
            )),
            Ok(AudioFormat::new(44_100, 2))
        );
        assert!(format_from_asbd(&asbd(48_000.0, 2, 0, 16)).is_err()); // int16
        assert!(format_from_asbd(&asbd(48_000.0, 0, float, 32)).is_err());
        assert!(format_from_asbd(&asbd(f64::NAN, 2, float, 32)).is_err());
        assert!(
            format_from_asbd(&asbd(48_000.0, 2, float | kAudioFormatFlagIsBigEndian, 32)).is_err()
        );
        let mut aac = asbd(48_000.0, 2, float, 32);
        aac.mFormatID = u32::from_be_bytes(*b"aac ");
        assert!(format_from_asbd(&aac).is_err());
    }

    #[test]
    fn app_names_come_from_the_outermost_app_bundle() {
        let chrome_helper = "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome \
                             Framework.framework/Versions/131.0/Helpers/Google Chrome Helper.app/\
                             Contents/MacOS/Google Chrome Helper";
        assert_eq!(app_bundle_name(chrome_helper), Some("Google Chrome"));
        assert_eq!(
            app_bundle_name("/Applications/Spotify.app/Contents/MacOS/Spotify"),
            Some("Spotify")
        );
        assert_eq!(
            app_bundle_name("/Applications/zoom.us.app/Contents/MacOS/zoom.us"),
            Some("zoom.us")
        );
        assert_eq!(app_bundle_name("/usr/bin/afplay"), None);
        assert_eq!(
            app_bundle_name(
                "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/\
                 com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU"
            ),
            None
        );
        assert_eq!(app_bundle_name("/Applications/.app/x"), None);
    }

    #[test]
    fn app_names_from_bundle_ids_skip_generic_words() {
        assert_eq!(bundle_id_name(Some("com.apple.Safari")), Some("Safari"));
        assert_eq!(bundle_id_name(Some("com.spotify.client")), Some("spotify"));
        assert_eq!(bundle_id_name(Some("us.zoom.xos")), Some("zoom"));
        assert_eq!(
            bundle_id_name(Some("com.google.Chrome.helper")),
            Some("Chrome")
        );
        assert_eq!(bundle_id_name(Some("com.apple.WebKit.GPU")), Some("WebKit"));
        assert_eq!(bundle_id_name(Some("Spotify")), Some("Spotify"));
        // Nothing better than the generic word: keep it rather than a reverse-DNS prefix.
        assert_eq!(bundle_id_name(Some("com.helper")), Some("helper"));
        assert_eq!(bundle_id_name(Some("helper")), Some("helper"));
        assert_eq!(bundle_id_name(Some("com.example.")), Some("example"));
        assert_eq!(bundle_id_name(Some("")), None);
        assert_eq!(bundle_id_name(None), None);
    }

    #[test]
    fn apps_are_sorted_and_deduplicated() {
        let app = |pid, name: &str| CaptureApp {
            pid,
            name: name.to_owned(),
        };
        let mut apps = vec![
            app(3, "zoom"),
            app(1, "Music"),
            app(2, "music"),
            app(1, "Music"),
        ];
        sort_and_dedup_apps(&mut apps);
        assert_eq!(apps, vec![app(1, "Music"), app(2, "music"), app(3, "zoom")]);
    }

    #[test]
    fn statuses_are_readable_and_mapped() {
        assert_eq!(
            status_string(kAudioHardwareIllegalOperationError),
            format!("'nope' ({kAudioHardwareIllegalOperationError})")
        );
        assert_eq!(status_string(-50), "-50");
        assert!(matches!(
            os_error("x", kAudioDevicePermissionsError),
            CaptureError::PermissionDenied(_)
        ));
        assert!(matches!(
            os_error("x", kAudioHardwareUnsupportedOperationError),
            CaptureError::Unsupported(_)
        ));
        assert!(matches!(os_error("x", -50), CaptureError::Backend(_)));
    }

    #[test]
    fn nope_means_permission_only_for_tap_creation() {
        // 'nope' from e.g. AudioDeviceStart is a generic failure, not a permission problem.
        assert!(matches!(
            os_error(
                "starting the tap device",
                kAudioHardwareIllegalOperationError
            ),
            CaptureError::Backend(_)
        ));
        assert!(matches!(
            tap_create_error(kAudioHardwareIllegalOperationError),
            CaptureError::PermissionDenied(_)
        ));
        // '!hog' is a permission error everywhere; other statuses keep their mapping.
        assert!(matches!(
            tap_create_error(kAudioDevicePermissionsError),
            CaptureError::PermissionDenied(_)
        ));
        assert!(matches!(
            tap_create_error(kAudioHardwareUnsupportedOperationError),
            CaptureError::Unsupported(_)
        ));
        assert!(matches!(tap_create_error(-50), CaptureError::Backend(_)));
    }

    #[test]
    fn impossible_pid_is_not_found() {
        // u32::MAX does not fit in a pid_t: no Core Audio call, just "unknown process".
        assert_eq!(translate_pid(u32::MAX), Ok(None));
        match open_process(u32::MAX) {
            Err(CaptureError::NotFound(_)) => {}
            // Older macOS rejects every entry point before looking at the pid.
            Err(CaptureError::Unsupported(_)) => assert!(ensure_supported().is_err()),
            Err(other) => panic!("expected NotFound, got {other:?}"),
            Ok(_) => panic!("opened a tap for pid u32::MAX"),
        }
    }

    #[test]
    fn list_apps_is_unsupported_before_macos_14_2() {
        if ensure_supported().is_err() {
            assert!(matches!(list_apps(), Err(CaptureError::Unsupported(_))));
        }
    }

    #[test]
    fn aggregate_description_has_the_audiocap_keys() {
        let key = |k: &CStr| CFString::from_str(&k.to_string_lossy());
        let string = |dict: &CFDictionary<CFString, CFType>, k: &CStr| {
            dict.get(&key(k))
                .and_then(|v| v.downcast::<CFString>().ok())
                .map(|v| v.to_string())
        };
        let number = |dict: &CFDictionary<CFString, CFType>, k: &CStr| {
            dict.get(&key(k))
                .and_then(|v| v.downcast::<CFNumber>().ok())
                .and_then(|v| v.as_i32())
        };

        let dict = aggregate_description("uid-x", "TAP-UUID");
        assert_eq!(dict.len(), 6);
        assert_eq!(
            string(&dict, kAudioAggregateDeviceNameKey).as_deref(),
            Some(DEVICE_NAME)
        );
        assert_eq!(
            string(&dict, kAudioAggregateDeviceUIDKey).as_deref(),
            Some("uid-x")
        );
        assert_eq!(number(&dict, kAudioAggregateDeviceIsPrivateKey), Some(1));
        assert_eq!(number(&dict, kAudioAggregateDeviceIsStackedKey), Some(0));
        assert_eq!(number(&dict, kAudioAggregateDeviceTapAutoStartKey), Some(1));

        let tap_list = dict
            .get(&key(kAudioAggregateDeviceTapListKey))
            .and_then(|v| v.downcast::<CFArray>().ok())
            .expect("TapList is a CFArray");
        assert_eq!(tap_list.len(), 1);
        // SAFETY: every element of a CFArray is a CF object.
        let tap_list: &CFArray<CFType> = unsafe { tap_list.cast_unchecked() };
        let entry = tap_list
            .get(0)
            .and_then(|v| v.downcast::<CFDictionary>().ok())
            .expect("the TapList entry is a CFDictionary");
        // SAFETY: the entry was built with CFString keys and CF object values.
        let entry: &CFDictionary<CFString, CFType> = unsafe { entry.cast_unchecked() };
        assert_eq!(entry.len(), 2);
        assert_eq!(
            string(entry, kAudioSubTapUIDKey).as_deref(),
            Some("TAP-UUID")
        );
        assert_eq!(number(entry, kAudioSubTapDriftCompensationKey), Some(1));
    }

    #[test]
    fn device_health_reports_the_first_failure() {
        let health = |nominal_rate| DeviceHealth {
            failure: AtomicU32::new(HEALTH_OK),
            format: AudioFormat::new(48_000, 2),
            nominal_rate,
        };
        let watched = health(Some(48_000.0));
        assert_eq!(watched.failure(), None);
        assert!(!watched.failed());
        // Properties the watch does not care about change nothing.
        watched.on_changed(SYSTEM_OBJECT, kAudioHardwarePropertyProcessObjectList);
        assert!(!watched.failed());
        watched.on_changed(SYSTEM_OBJECT, kAudioHardwarePropertyServiceRestarted);
        assert!(watched.failed());
        assert!(watched.failure().is_some_and(|r| r.contains("restarted")));
        watched.fail(HEALTH_FORMAT_CHANGED);
        assert!(
            watched.failure().is_some_and(|r| r.contains("restarted")),
            "the first reason is kept"
        );
        // A device that can no longer be asked whether it is alive is gone.
        let gone = health(None);
        gone.on_changed(kAudioObjectUnknown, kAudioDevicePropertyDeviceIsAlive);
        assert!(gone.failure().is_some_and(|r| r.contains("went away")));
        // A stream format that cannot be read is not taken for a change.
        let unread = health(None);
        unread.on_changed(kAudioObjectUnknown, kAudioStreamPropertyVirtualFormat);
        unread.on_changed(kAudioObjectUnknown, kAudioDevicePropertyNominalSampleRate);
        assert!(!unread.failed());
    }

    #[test]
    fn mute_behavior_silences_local_output_while_tapped() {
        assert_eq!(MUTE_BEHAVIOR, CATapMuteBehavior::MutedWhenTapped);
    }

    /// Captures real system audio for half a second. Needs macOS 14.2+ and the "System Audio
    /// Recording" permission (a prompt on first run), so it only runs on request:
    /// `cargo test -p hfa-capture -- --ignored`.
    #[test]
    #[ignore = "needs a Mac with the System Audio Recording permission"]
    fn captures_system_audio() {
        let mut source = open_system(true).expect("open system tap");
        let format = source.format();
        assert!(
            format.sample_rate >= 8_000 && format.channels >= 1,
            "{format:?}"
        );
        assert!(source.describe().contains("except this app"));
        let (sink, _source) = crate::pcm_ring(format.sample_rate as usize * 4);
        let overruns = sink.stats();
        source.start(sink).expect("start");
        let (sink2, _unused) = crate::pcm_ring(16);
        assert_eq!(source.start(sink2), Err(CaptureError::AlreadyRunning));
        std::thread::sleep(std::time::Duration::from_millis(500));
        source.stop();
        source.stop(); // idempotent
        assert_eq!(overruns.count(), 0);
        // Restart after stop rebuilds the tap and aggregate device.
        let (sink3, _source3) = crate::pcm_ring(format.sample_rate as usize * 4);
        source.start(sink3).expect("restart");
        drop(source); // Drop stops and releases everything.
    }

    /// Talks to the real audio server: must never panic, whatever the OS version/permissions.
    #[test]
    fn os_entry_points_do_not_panic() {
        let caps = capabilities();
        assert_eq!(caps.system_mix, ensure_supported().is_ok());
        assert!(!caps.notes.is_empty());
        let _ = list_apps();
        let _ = translate_pid(std::process::id());
    }
}
