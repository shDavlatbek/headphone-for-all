//! Linux capture backend on PipeWire (`pipewire` crate).
//!
//! Every capture source owns one dedicated thread (`hfa-pw-capture`) that runs a PipeWire
//! `MainLoop` + `Context` + `Core` and one input `Stream` negotiated as **F32LE interleaved,
//! 48 kHz stereo** (PipeWire's stream adapter converts whatever the graph runs at). The stream
//! is connected when the source is *opened* so that the negotiated format is known before
//! [`CaptureSource::start`] (and connection problems surface as an open error); until `start`
//! hands over the [`PcmSink`], captured buffers are dropped.
//!
//! Three modes:
//!
//! - **System mix** ([`open_system`]`(false)`): the stream sets `stream.capture.sink = true`
//!   and is auto-connected by the session manager to the monitor of the default sink (it
//!   follows the default sink when it changes).
//! - **System mix excluding this process** ([`open_system`]`(true)`): the sink monitor would
//!   also contain our own playback (hub + sender on one machine would loop), so instead the
//!   stream is left unconnected and this module links the output ports of every application
//!   playback stream (`media.class = Stream/Output/Audio`) whose process id is **not** ours
//!   straight into the stream's input ports, using the registry and the `link-factory`.
//!   PipeWire mixes several links into one input port. Links follow streams as they appear
//!   and disappear; a link that fails or is removed by someone else is recreated (a few
//!   times, then given up until its ports change).
//! - **One process** ([`open_process`]): the same linking, restricted to the playback streams
//!   of that process and its descendants (browsers play from child processes).
//!
//! **Relays are never linked.** The playback half of a loopback, filter-chain, echo-cancel or
//! combine-stream node (`node.link-group` set), a virtual stream (`node.virtual = true`) and
//! the output of a client that also owns an `Audio/Sink` node (EasyEffects-style virtual
//! sinks) only forward what applications played into a sink. Linking them would capture those
//! applications twice (comb filtering, double level) and would bring our own playback back in
//! through the relay when the default sink is virtual. The applications behind a relay are
//! captured from their own streams instead.
//!
//! The process id of a playback stream is resolved from its owning client (the node's and the
//! client's `client.api` / `pipewire.access` properties):
//!
//! - native PipeWire clients: the kernel-verified `pipewire.sec.pid` (host pid namespace),
//!   falling back to `application.process.id`;
//! - `pipewire-pulse` clients: the node's `application.process.id` (the pulse server's own
//!   `sec.pid` says nothing about the application), else the client's;
//! - sandboxed (Flatpak, `pipewire.access = flatpak`) clients that only report a pid of their
//!   own pid namespace: mapped to the host pid through `/proc/*/status` `NSpid` plus the
//!   sandbox's Flatpak app id; streams that cannot be mapped unambiguously are not listed and
//!   not matched by pid (system-excl still links them: they are never this process).
//!
//! Real-time rules: stream callbacks run on the capture thread's main loop (no
//! `RT_PROCESS`). The `process` callback only dequeues a buffer, converts little-endian `f32`
//! bytes through a preallocated scratch buffer and pushes them into the lock-free
//! [`PcmSink`]: no allocation, no lock, no logging, no syscall.
//!
//! This file is owned by `feat/capture-linux`. It exposes exactly the four `pub(crate)`
//! functions of the platform-module interface (`docs/CONTRACTS.md` §5).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::{Rc, Weak};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::thread::JoinHandle;
use std::time::Duration;

use hfa_audio::AudioFormat;
use pipewire as pw;
use pw::proxy::ProxyT;
use pw::spa;
use pw::spa::pod::Pod;
use pw::stream::{StreamFlags, StreamState};
use pw::types::ObjectType;

use crate::ring::PcmSink;
use crate::{Capabilities, CaptureApp, CaptureError, CaptureSource, Result};

/// `node.name` / `application.name` of our capture streams.
const APP_NAME: &str = "headphone-for-all";
/// The format we ask PipeWire for (its stream adapter converts to it).
const REQUESTED_FORMAT: AudioFormat = AudioFormat::INTERNAL;
/// Media class of application playback streams.
const OUTPUT_STREAM_CLASS: &str = "Stream/Output/Audio";
/// How long `open_*` waits for the capture thread to connect and negotiate.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
/// If no format was negotiated after this long (e.g. no sink exists yet), the source reports
/// [`REQUESTED_FORMAT`], which is the only format the stream offers.
const NEGOTIATION_GRACE: Duration = Duration::from_secs(2);
/// Timeout of each registry round trip in [`list_apps`].
const ROUNDTRIP_TIMEOUT: Duration = Duration::from_secs(3);
/// Samples converted per push from the `process` callback (a multiple of 1, 2, 4, 6, 8).
const SCRATCH_SAMPLES: usize = 4096 - 4096 % 24;
/// How often a lost capture link is recreated before it is given up (until its ports change).
const MAX_LINK_RETRIES: u32 = 3;
/// Upper bound on the walk up the process tree (guards against cycles in `/proc` races).
const MAX_PROCESS_DEPTH: usize = 64;

const NOTES: &str = "PipeWire backend. \"system\" captures the monitor of the default sink \
(it follows default-sink changes). \"system-excl\" and per-app capture link application playback \
streams (Stream/Output/Audio) directly into the capture stream, so they exclude this process. \
Relay streams (loopback, filter-chain, echo-cancel, combine and virtual-sink outputs) are not \
linked because the applications feeding them are already captured directly; sound that reaches \
the sink only through such a relay without an application stream behind it (e.g. a microphone \
loopback) or that is written straight to ALSA hardware is therefore missed. Per-app capture \
includes child processes; sandboxed (Flatpak) PulseAudio clients are listed only when their host \
pid can be determined. Requires a running PipeWire daemon (pipewire-pulse setups included) and \
libpipewire-0.3 at run time.";

// ---------------------------------------------------------------------------------------------
// Platform-module interface
// ---------------------------------------------------------------------------------------------

/// Capture capabilities on Linux: everything works when a PipeWire daemon is reachable.
pub(crate) fn capabilities() -> Capabilities {
    match Connection::open(None) {
        Ok(_) => Capabilities {
            system_mix: true,
            per_app: true,
            mutes_local_output: false,
            notes: NOTES.to_owned(),
        },
        Err(e) => Capabilities {
            system_mix: false,
            per_app: false,
            mutes_local_output: false,
            notes: format!("PipeWire is not available: {e}. {NOTES}"),
        },
    }
}

/// Opens system-mix capture; with `exclude_self` the current process is excluded.
pub(crate) fn open_system(exclude_self: bool) -> Result<Box<dyn CaptureSource>> {
    if exclude_self {
        let own = std::process::id();
        let source = PipeWireCapture::open(
            Mode::Linked(NodeFilter::AllExcept(own)),
            "System audio except headphone-for-all (PipeWire application streams)".to_owned(),
            None,
        )?;
        Ok(Box::new(source))
    } else {
        let source = PipeWireCapture::open(
            Mode::Monitor,
            "System audio (PipeWire monitor of the default sink)".to_owned(),
            None,
        )?;
        Ok(Box::new(source))
    }
}

/// Opens capture of one process and its descendants.
///
/// The process must exist; it does not need to be playing yet (its streams are linked as
/// soon as they appear).
pub(crate) fn open_process(pid: u32) -> Result<Box<dyn CaptureSource>> {
    if pid == 0 {
        return Err(CaptureError::InvalidArgument(
            "pid 0 is not a process".to_owned(),
        ));
    }
    if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
        return Err(CaptureError::NotFound(format!(
            "process {pid} does not exist"
        )));
    }
    let description = match process_name(pid) {
        Some(name) => format!("Process {pid} ({name}) via PipeWire"),
        None => format!("Process {pid} via PipeWire"),
    };
    let source = PipeWireCapture::open(
        Mode::Linked(NodeFilter::ProcessTree(pid)),
        description,
        None,
    )?;
    Ok(Box::new(source))
}

/// Lists processes that currently have a PipeWire playback stream (paused ones included),
/// one entry per process, sorted by name. This process is never listed.
pub(crate) fn list_apps() -> Result<Vec<CaptureApp>> {
    list_apps_on(None)
}

// ---------------------------------------------------------------------------------------------
// Pure helpers (unit-tested)
// ---------------------------------------------------------------------------------------------

/// Parses a process id property (`application.process.id`, `pipewire.sec.pid`). 0 is invalid.
fn parse_pid(value: &str) -> Option<u32> {
    value.trim().parse::<u32>().ok().filter(|&pid| pid != 0)
}

/// Extracts the parent pid from the contents of `/proc/<pid>/stat`.
///
/// The second field (`comm`) is parenthesised and may itself contain spaces and `)`, so the
/// parse starts after the **last** `)`: `pid (comm) state ppid ...`.
fn parse_ppid(stat: &str) -> Option<u32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

/// Parent pid of `pid`, from `/proc`.
fn read_ppid(pid: u32) -> Option<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| parse_ppid(&stat))
}

/// Short process name (`/proc/<pid>/comm`).
fn process_name(pid: u32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let comm = comm.trim();
    (!comm.is_empty()).then(|| comm.to_owned())
}

/// Whether `pid` is a (transitive) child of `ancestor`, walking up with `parent_of`.
fn is_descendant(pid: u32, ancestor: u32, parent_of: &dyn Fn(u32) -> Option<u32>) -> bool {
    let mut current = pid;
    for _ in 0..MAX_PROCESS_DEPTH {
        match parent_of(current) {
            Some(parent) if parent == ancestor => return true,
            Some(parent) if parent != 0 && parent != current => current = parent,
            _ => return false,
        }
    }
    false
}

/// Parses the `NSpid:` line of `/proc/<pid>/status`: the pid in every pid namespace the
/// process is in, from the reader's namespace down to the innermost one.
fn parse_nspid(status: &str) -> Option<Vec<u32>> {
    let line = status.lines().find_map(|l| l.strip_prefix("NSpid:"))?;
    let pids: Option<Vec<u32>> = line.split_whitespace().map(|p| p.parse().ok()).collect();
    pids.filter(|p| !p.is_empty())
}

/// The Flatpak application id in a sandbox's `/.flatpak-info` (`[Application]` `name=`).
fn parse_flatpak_info_app_id(info: &str) -> Option<&str> {
    let mut in_application = false;
    for line in info.lines().map(str::trim) {
        if line.starts_with('[') {
            in_application = line == "[Application]";
        } else if in_application {
            if let Some(name) = line.strip_prefix("name=") {
                return Some(name.trim()).filter(|n| !n.is_empty());
            }
        }
    }
    None
}

/// The Flatpak application id from `/proc/<pid>/cgroup` (systemd scope
/// `app-flatpak-<app id>-<number>.scope`).
fn parse_flatpak_cgroup_app_id(cgroup: &str) -> Option<String> {
    cgroup.lines().find_map(|line| {
        let scope = line.rsplit('/').next()?;
        let rest = scope.strip_prefix("app-flatpak-")?.strip_suffix(".scope")?;
        let (app_id, instance) = rest.rsplit_once('-')?;
        (!app_id.is_empty() && instance.bytes().all(|b| b.is_ascii_digit()))
            .then(|| app_id.replace("\\x2d", "-"))
    })
}

/// Whether host process `pid` is the sandboxed process that reported `sandbox_pid` (its
/// innermost namespace pid) and, if known, belongs to Flatpak app `app_id`.
///
/// `read(pid, file)` returns the contents of `/proc/<pid>/<file>`.
fn is_sandboxed_as(
    pid: u32,
    sandbox_pid: u32,
    app_id: Option<&str>,
    read: &dyn Fn(u32, &str) -> Option<String>,
) -> bool {
    let Some(nspid) = read(pid, "status").and_then(|s| parse_nspid(&s)) else {
        return false;
    };
    if nspid.len() < 2 || nspid.first() != Some(&pid) || nspid.last() != Some(&sandbox_pid) {
        return false;
    }
    let Some(app_id) = app_id else {
        return true;
    };
    let from_info = read(pid, "root/.flatpak-info")
        .and_then(|info| parse_flatpak_info_app_id(&info).map(str::to_owned));
    let actual =
        from_info.or_else(|| read(pid, "cgroup").and_then(|c| parse_flatpak_cgroup_app_id(&c)));
    actual.as_deref() == Some(app_id)
}

/// Finds the host pid of the sandboxed process that reported `sandbox_pid` among `pids`.
/// `None` when there is no match or when the match is ambiguous (never guess).
fn find_host_pid(
    sandbox_pid: u32,
    app_id: Option<&str>,
    pids: impl IntoIterator<Item = u32>,
    read: &dyn Fn(u32, &str) -> Option<String>,
) -> Option<u32> {
    let mut found = None;
    for pid in pids {
        if is_sandboxed_as(pid, sandbox_pid, app_id, read) {
            if found.is_some() {
                return None;
            }
            found = Some(pid);
        }
    }
    found
}

/// Process-table queries the graph model needs (`/proc` in production, fakes in tests).
trait Procs {
    /// Parent pid of `pid`.
    fn parent_of(&self, pid: u32) -> Option<u32>;
    /// Host pid of the sandboxed process that reports `sandbox_pid` inside its own pid
    /// namespace (Flatpak app `app_id`, if known).
    fn host_pid(&self, sandbox_pid: u32, app_id: Option<&str>) -> Option<u32>;
}

/// [`Procs`] on `/proc`, caching sandbox-to-host pid mappings (re-validated on use).
#[derive(Debug, Default)]
struct ProcFs {
    sandbox_cache: RefCell<HashMap<(u32, Option<String>), u32>>,
}

/// Reads `/proc/<pid>/<file>`.
fn read_proc_file(pid: u32, file: &str) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/{file}")).ok()
}

/// The pids currently visible in `/proc`.
fn proc_pids() -> Vec<u32> {
    std::fs::read_dir("/proc")
        .map(|dir| {
            dir.filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

impl Procs for ProcFs {
    fn parent_of(&self, pid: u32) -> Option<u32> {
        read_ppid(pid)
    }

    fn host_pid(&self, sandbox_pid: u32, app_id: Option<&str>) -> Option<u32> {
        let key = (sandbox_pid, app_id.map(str::to_owned));
        let cached = self.sandbox_cache.borrow().get(&key).copied();
        if let Some(host) = cached {
            if is_sandboxed_as(host, sandbox_pid, app_id, &read_proc_file) {
                return Some(host);
            }
        }
        let found = find_host_pid(sandbox_pid, app_id, proc_pids(), &read_proc_file);
        let mut cache = self.sandbox_cache.borrow_mut();
        match found {
            Some(host) => cache.insert(key, host),
            None => cache.remove(&key),
        };
        found
    }
}

/// Who owns a playback stream, as far as the graph tells.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Owner {
    /// The node's or its client's info has not arrived yet: do not act on the stream.
    Pending,
    /// A pid in the host pid namespace (kernel-verified, or reported by an unsandboxed app).
    Host(u32),
    /// A pid a sandboxed app reported from inside its own pid namespace.
    Sandboxed {
        /// The pid inside the sandbox (often 2).
        pid: u32,
        /// The Flatpak app id, if the client carries one.
        app_id: Option<String>,
    },
    /// No pid at all.
    Unknown,
}

impl Owner {
    /// The owner's pid in the host namespace, if it can be determined.
    fn host_pid(&self, procs: &dyn Procs) -> Option<u32> {
        match self {
            Owner::Host(pid) => Some(*pid),
            Owner::Sandboxed { pid, app_id } => procs.host_pid(*pid, app_id.as_deref()),
            Owner::Pending | Owner::Unknown => None,
        }
    }
}

/// Which application streams a linked capture takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeFilter {
    /// Every application stream except those of this process id.
    AllExcept(u32),
    /// The streams of this process and of its descendants.
    ProcessTree(u32),
}

impl NodeFilter {
    fn matches(self, owner: &Owner, procs: &dyn Procs) -> bool {
        match self {
            NodeFilter::AllExcept(excluded) => match owner {
                Owner::Pending => false,
                Owner::Host(pid) => *pid != excluded,
                // A sandboxed stream belongs to another app even when its host pid is
                // unknown (this process is not the sandboxed client).
                Owner::Sandboxed { .. } => owner.host_pid(procs) != Some(excluded),
                // Our own streams always carry a pid.
                Owner::Unknown => true,
            },
            NodeFilter::ProcessTree(root) => match owner.host_pid(procs) {
                Some(pid) => pid == root || is_descendant(pid, root, &|p| procs.parent_of(p)),
                None => false,
            },
        }
    }
}

/// Where a source channel goes when the capture stream has no port of the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
    /// Mono / centre channels go to both sides.
    Both,
}

/// Classifies a SPA channel name (`audio.channel` of a port). `None` = drop (LFE).
fn channel_side(channel: &str) -> Option<Side> {
    match channel {
        "LFE" | "LFE2" => None,
        "FL" | "SL" | "RL" | "FLC" | "RLC" | "TFL" | "TRL" | "TSL" | "FLW" | "FLH" | "TFLC"
        | "LLFE" | "BLC" => Some(Side::Left),
        "FR" | "SR" | "RR" | "FRC" | "RRC" | "TFR" | "TRR" | "TSR" | "FRW" | "FRH" | "TFRC"
        | "RLFE" | "BRC" => Some(Side::Right),
        other => match other
            .strip_prefix("AUX")
            .and_then(|n| n.parse::<u32>().ok())
        {
            Some(n) if n % 2 == 0 => Some(Side::Left),
            Some(_) => Some(Side::Right),
            // MONO, FC, RC, TC, TFC, TRC, BC, UNK, NA, ...
            None => Some(Side::Both),
        },
    }
}

/// Input port ids of our capture stream that a source port of `channel` is linked to.
///
/// `inputs` maps our input ports' `audio.channel` to their port id. An exact channel match
/// wins; otherwise left/right/centre routing onto `FL`/`FR` (or `MONO` for a mono capture).
fn route_channel(channel: &str, inputs: &BTreeMap<String, u32>) -> Vec<u32> {
    if let Some(&port) = inputs.get(channel) {
        return vec![port];
    }
    if let Some(&mono) = inputs.get("MONO") {
        return match channel_side(channel) {
            Some(_) => vec![mono],
            None => Vec::new(),
        };
    }
    let left = inputs.get("FL").copied();
    let right = inputs.get("FR").copied();
    match channel_side(channel) {
        Some(Side::Left) => left.into_iter().collect(),
        Some(Side::Right) => right.into_iter().collect(),
        Some(Side::Both) => left.into_iter().chain(right).collect(),
        None => Vec::new(),
    }
}

/// Validates a negotiated rate/channel count.
fn audio_format_from(rate: u32, channels: u32) -> Result<AudioFormat> {
    if !(1..=768_000).contains(&rate) {
        return Err(CaptureError::Format(format!(
            "PipeWire negotiated an invalid sample rate {rate}"
        )));
    }
    match u16::try_from(channels) {
        Ok(ch) if (1..=64).contains(&ch) => Ok(AudioFormat::new(rate, ch)),
        _ => Err(CaptureError::Format(format!(
            "PipeWire negotiated an invalid channel count {channels}"
        ))),
    }
}

/// Reads sample `index` of one interleaved little-endian `f32` frame (0.0 if out of range).
fn frame_sample(frame: &[u8], index: usize) -> f32 {
    frame
        .get(index * 4..index * 4 + 4)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map_or(0.0, f32::from_le_bytes)
}

/// Converts one frame of `in_ch` little-endian `f32` samples into `out` (`out.len()`
/// channels): copy, mono upmix, average downmix to mono, or first-N-channels.
fn convert_frame(frame: &[u8], in_ch: usize, out: &mut [f32]) {
    let out_ch = out.len();
    if in_ch == out_ch {
        for (c, o) in out.iter_mut().enumerate() {
            *o = frame_sample(frame, c);
        }
    } else if in_ch == 1 {
        out.fill(frame_sample(frame, 0));
    } else if out_ch == 1 {
        let sum: f32 = (0..in_ch).map(|c| frame_sample(frame, c)).sum();
        out[0] = sum / in_ch as f32;
    } else {
        for (c, o) in out.iter_mut().enumerate() {
            *o = if c < in_ch {
                frame_sample(frame, c)
            } else {
                0.0
            };
        }
    }
}

/// Converts interleaved little-endian `f32` bytes with `in_ch` channels into `out_ch`
/// channels, calling `emit` with whole-frame blocks of at most `scratch.len()` samples.
/// A trailing partial frame is ignored. Allocation-free (real-time safe).
fn convert_f32le(
    bytes: &[u8],
    in_ch: usize,
    out_ch: usize,
    scratch: &mut [f32],
    mut emit: impl FnMut(&[f32]),
) {
    let in_ch = in_ch.max(1);
    let out_ch = out_ch.max(1);
    let frames_per_block = scratch.len() / out_ch;
    if frames_per_block == 0 {
        return;
    }
    let mut frames = bytes.chunks_exact(in_ch * 4);
    loop {
        let mut filled = 0;
        for frame in frames.by_ref().take(frames_per_block) {
            convert_frame(frame, in_ch, &mut scratch[filled..filled + out_ch]);
            filled += out_ch;
        }
        if filled == 0 {
            break;
        }
        emit(&scratch[..filled]);
    }
}

/// A PipeWire client (its process identity).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ClientModel {
    /// `pipewire.sec.pid`: the kernel-verified peer pid, in the host pid namespace (for
    /// `pipewire-pulse` clients this is the pulse server itself).
    sec_pid: Option<u32>,
    /// `application.process.id`: what the application reported (`getpid()` in its own
    /// pid namespace).
    app_pid: Option<u32>,
    /// `application.name`.
    app_name: Option<String>,
    /// `client.api` (`pipewire-pulse` for PulseAudio clients).
    api: Option<String>,
    /// The client runs in a sandbox (`pipewire.access = flatpak` or a portal app id).
    sandboxed: bool,
    /// `pipewire.access.portal.app_id` (the Flatpak app id).
    app_id: Option<String>,
    /// The client info (full properties) has arrived.
    info_seen: bool,
}

impl ClientModel {
    /// Merges client properties (global or info); absent keys keep their value.
    fn apply_props<'a>(&mut self, get: impl Fn(&str) -> Option<&'a str>) {
        if let Some(pid) = get("pipewire.sec.pid").and_then(parse_pid) {
            self.sec_pid = Some(pid);
        }
        if let Some(pid) = get("application.process.id").and_then(parse_pid) {
            self.app_pid = Some(pid);
        }
        if let Some(name) = get("application.name") {
            self.app_name = Some(name.to_owned());
        }
        if let Some(api) = get("client.api") {
            self.api = Some(api.to_owned());
        }
        if let Some(app_id) = get("pipewire.access.portal.app_id").filter(|a| !a.is_empty()) {
            self.app_id = Some(app_id.to_owned());
            self.sandboxed = true;
        }
        if get("pipewire.access") == Some("flatpak") {
            self.sandboxed = true;
        }
    }

    /// A PulseAudio client served by `pipewire-pulse`.
    fn is_pulse(&self) -> bool {
        self.api.as_deref() == Some("pipewire-pulse")
    }
}

/// An application playback stream node (`Stream/Output/Audio`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct NodeModel {
    /// `client.id` (global props).
    client_id: Option<u32>,
    /// `application.process.id` from the node info (PulseAudio clients).
    app_pid: Option<u32>,
    /// `application.name`.
    app_name: Option<String>,
    /// `node.name`.
    node_name: Option<String>,
    /// Part of a loopback/filter pair (`node.link-group`) or virtual (`node.virtual`): it
    /// relays audio that applications played into a sink.
    relay: bool,
    /// The node info (full properties) has arrived, so its pid can be trusted.
    info_seen: bool,
}

impl NodeModel {
    /// Merges node properties (global or info); absent keys keep their value.
    fn apply_props<'a>(&mut self, get: impl Fn(&str) -> Option<&'a str>) {
        if let Some(id) = get("client.id").and_then(|v| v.trim().parse().ok()) {
            self.client_id = Some(id);
        }
        if let Some(pid) = get("application.process.id").and_then(parse_pid) {
            self.app_pid = Some(pid);
        }
        if let Some(name) = get("application.name") {
            self.app_name = Some(name.to_owned());
        }
        if let Some(name) = get("node.name") {
            self.node_name = Some(name.to_owned());
        }
        if get("node.link-group").is_some_and(|g| !g.is_empty())
            || get("node.virtual") == Some("true")
        {
            self.relay = true;
        }
    }
}

/// A port (global props).
#[derive(Debug, Clone, PartialEq, Eq)]
struct PortModel {
    node_id: u32,
    output: bool,
    monitor: bool,
    channel: String,
}

impl PortModel {
    /// Builds a port from its global properties; `None` if it is not a usable audio port.
    fn from_props<'a>(get: impl Fn(&str) -> Option<&'a str>) -> Option<Self> {
        let node_id = get("node.id")?.trim().parse().ok()?;
        let output = match get("port.direction")? {
            "out" => true,
            "in" => false,
            _ => return None,
        };
        Some(Self {
            node_id,
            output,
            monitor: get("port.monitor") == Some("true"),
            channel: get("audio.channel").unwrap_or("MONO").to_owned(),
        })
    }
}

/// One link from an application output port into one of our input ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct PlannedLink {
    out_node: u32,
    out_port: u32,
    in_node: u32,
    in_port: u32,
}

/// The part of the PipeWire graph this backend cares about, as plain data.
#[derive(Debug, Default)]
struct GraphModel {
    clients: BTreeMap<u32, ClientModel>,
    nodes: BTreeMap<u32, NodeModel>,
    ports: BTreeMap<u32, PortModel>,
    /// `Audio/Sink` nodes and their owning client (`client.id`).
    sinks: BTreeMap<u32, Option<u32>>,
}

impl GraphModel {
    /// Who owns a playback stream node (see the module docs for the resolution order).
    fn node_owner(&self, node_id: u32) -> Owner {
        let Some(node) = self.nodes.get(&node_id) else {
            return Owner::Pending;
        };
        if !node.info_seen {
            return Owner::Pending;
        }
        let client = match node.client_id {
            Some(id) => match self.clients.get(&id) {
                Some(client) if client.info_seen => Some(client),
                _ => return Owner::Pending,
            },
            None => None,
        };
        let reported = node.app_pid.or_else(|| client.and_then(|c| c.app_pid));
        let sandboxed = client.is_some_and(|c| c.sandboxed);
        let from_report = |pid: Option<u32>| match pid {
            Some(pid) if sandboxed => Owner::Sandboxed {
                pid,
                app_id: client.and_then(|c| c.app_id.clone()),
            },
            Some(pid) => Owner::Host(pid),
            None => Owner::Unknown,
        };
        match client {
            // The pulse server's sec.pid is its own: only the reported pid names the app.
            Some(c) if c.is_pulse() => from_report(reported),
            Some(c) => match c.sec_pid {
                Some(pid) => Owner::Host(pid),
                None => from_report(reported),
            },
            None => from_report(reported),
        }
    }

    /// Whether a playback stream only relays audio played into a sink (loopback,
    /// filter-chain, virtual-sink outputs): its sources are captured from their own streams.
    fn is_relay(&self, node_id: u32) -> bool {
        let Some(node) = self.nodes.get(&node_id) else {
            return false;
        };
        node.relay
            || node
                .client_id
                .is_some_and(|client| self.sinks.values().any(|&owner| owner == Some(client)))
    }

    /// Display name of a playback stream node's application.
    fn node_app_name(&self, node_id: u32) -> Option<String> {
        let node = self.nodes.get(&node_id)?;
        let client = node.client_id.and_then(|id| self.clients.get(&id));
        node.app_name
            .clone()
            .or_else(|| client.and_then(|c| c.app_name.clone()))
            .or_else(|| node.node_name.clone())
    }

    /// Processes with playback streams, deduplicated by host pid, `own_pid` and relays
    /// excluded, sorted by name (case-insensitive) then pid. Streams whose host pid cannot be
    /// determined are left out rather than listed under a wrong pid.
    fn apps(&self, own_pid: u32, procs: &dyn Procs) -> Vec<CaptureApp> {
        let mut by_pid: BTreeMap<u32, String> = BTreeMap::new();
        for &node_id in self.nodes.keys() {
            if self.is_relay(node_id) {
                continue;
            }
            let Some(pid) = self.node_owner(node_id).host_pid(procs) else {
                continue;
            };
            if pid == own_pid {
                continue;
            }
            let name = self
                .node_app_name(node_id)
                .unwrap_or_else(|| format!("pid {pid}"));
            by_pid.entry(pid).or_insert(name);
        }
        let mut apps: Vec<CaptureApp> = by_pid
            .into_iter()
            .map(|(pid, name)| CaptureApp { pid, name })
            .collect();
        apps.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.pid.cmp(&b.pid))
        });
        apps
    }

    /// The links a linked capture into `own_node` should have right now.
    fn plan_links(
        &self,
        own_node: u32,
        filter: NodeFilter,
        procs: &dyn Procs,
    ) -> BTreeSet<PlannedLink> {
        let inputs: BTreeMap<String, u32> = self
            .ports
            .iter()
            .filter(|(_, p)| p.node_id == own_node && !p.output && !p.monitor)
            .map(|(&id, p)| (p.channel.clone(), id))
            .collect();
        let mut planned = BTreeSet::new();
        if inputs.is_empty() {
            return planned;
        }
        for &node_id in self.nodes.keys() {
            if node_id == own_node || self.is_relay(node_id) {
                continue;
            }
            if !filter.matches(&self.node_owner(node_id), procs) {
                continue;
            }
            let outputs = self
                .ports
                .iter()
                .filter(|(_, p)| p.node_id == node_id && p.output && !p.monitor);
            for (&out_port, port) in outputs {
                for in_port in route_channel(&port.channel, &inputs) {
                    planned.insert(PlannedLink {
                        out_node: node_id,
                        out_port,
                        in_node: own_node,
                        in_port,
                    });
                }
            }
        }
        planned
    }
}

// ---------------------------------------------------------------------------------------------
// PipeWire plumbing
// ---------------------------------------------------------------------------------------------

fn backend(what: &str, e: impl std::fmt::Display) -> CaptureError {
    CaptureError::Backend(format!("PipeWire: {what}: {e}"))
}

/// Whether a core `error` event (on `PW_ID_CORE`) means the connection to the daemon is gone.
///
/// libpipewire reports a broken socket as `-EPIPE` (hang-up) or the failing socket call's
/// error with the message "connection error". Other core errors answer one of our requests
/// (for example `-ENOENT` "unknown resource" for a `destroy` of an object the server already
/// removed) and leave the connection working.
fn is_connection_error(res: i32) -> bool {
    matches!(
        res.checked_neg(),
        Some(libc::EPIPE | libc::ECONNRESET | libc::ENOTCONN | libc::ECONNREFUSED | libc::EPROTO)
    )
}

/// A connection to the PipeWire daemon, owned by one thread. Fields drop in declaration
/// order: core, then context, then main loop.
struct Connection {
    core: pw::core::CoreRc,
    _context: pw::context::ContextRc,
    mainloop: pw::main_loop::MainLoopRc,
}

impl Connection {
    /// Connects to the daemon (`remote` = `remote.name`, `None` = the default socket).
    fn open(remote: Option<&str>) -> Result<Self> {
        pw::init();
        let mainloop =
            pw::main_loop::MainLoopRc::new(None).map_err(|e| backend("create main loop", e))?;
        let context = pw::context::ContextRc::new(&mainloop, None)
            .map_err(|e| backend("create context", e))?;
        let props = remote.map(|name| pw::properties::properties! { "remote.name" => name });
        let core = context.connect_rc(props).map_err(|e| {
            CaptureError::Backend(format!(
                "cannot connect to the PipeWire daemon (is PipeWire running for this user?): {e}"
            ))
        })?;
        Ok(Self {
            core,
            _context: context,
            mainloop,
        })
    }

    /// Runs the loop until the server has processed everything sent so far.
    fn roundtrip(&self, timeout: Duration) -> Result<()> {
        #[derive(Clone, Copy, PartialEq)]
        enum Outcome {
            Pending,
            Done,
            TimedOut,
            Failed,
        }
        let outcome = Rc::new(std::cell::Cell::new(Outcome::Pending));
        let pending = self.core.sync(0).map_err(|e| backend("sync", e))?;
        let _core_listener = self
            .core
            .add_listener_local()
            .done({
                let outcome = Rc::clone(&outcome);
                let mainloop = self.mainloop.clone();
                move |id, seq| {
                    if id == pw::core::PW_ID_CORE && seq == pending {
                        outcome.set(Outcome::Done);
                        mainloop.quit();
                    }
                }
            })
            .error({
                let outcome = Rc::clone(&outcome);
                let mainloop = self.mainloop.clone();
                move |id, _seq, res, message| {
                    if id == pw::core::PW_ID_CORE && is_connection_error(res) {
                        tracing::warn!(res, message, "PipeWire connection lost");
                        outcome.set(Outcome::Failed);
                        mainloop.quit();
                    } else {
                        tracing::debug!(id, res, message, "PipeWire error");
                    }
                }
            })
            .register();
        let timer = self.mainloop.loop_().add_timer({
            let outcome = Rc::clone(&outcome);
            let mainloop = self.mainloop.clone();
            move |_| {
                outcome.set(Outcome::TimedOut);
                mainloop.quit();
            }
        });
        let _ = timer.update_timer(Some(timeout), None);
        while outcome.get() == Outcome::Pending {
            self.mainloop.run();
        }
        match outcome.get() {
            Outcome::Done | Outcome::Pending => Ok(()),
            Outcome::TimedOut => Err(CaptureError::Backend(
                "PipeWire daemon did not answer in time".to_owned(),
            )),
            Outcome::Failed => Err(CaptureError::Backend(
                "PipeWire connection failed".to_owned(),
            )),
        }
    }
}

/// Proxies bound to read full info properties. Listener before proxy (drop order).
#[allow(dead_code)] // the fields only keep the proxies alive
enum Bound {
    Node(pw::node::NodeListener, pw::node::Node),
    Client(pw::client::ClientListener, pw::client::Client),
}

/// A link we created went away without us removing it: it failed (proxy error, link state
/// `error`) or the server removed it (someone else deleted it). Sent from the link's
/// listeners to the capture loop, which drops the entry and relinks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LinkLost {
    key: PlannedLink,
    /// Identifies the link object, so a late report cannot hit a newer link of the same key.
    serial: u64,
}

/// A link we created. Fields drop in declaration order: the listeners are unregistered
/// before the proxy is dropped, and dropping the proxy destroys the link exactly once
/// (`pw_proxy_destroy` sends the `destroy` itself unless the server already removed it).
struct OwnedLink {
    _info_listener: pw::link::LinkListener,
    _proxy_listener: pw::proxy::ProxyListener,
    _link: pw::link::Link,
    serial: u64,
}

/// The linking state of a linked capture.
struct LinkCapture {
    core: pw::core::CoreRc,
    filter: NodeFilter,
    /// Our capture stream's node id, once the stream is connected.
    own_node: Option<u32>,
    links: HashMap<PlannedLink, OwnedLink>,
    /// How often each planned link was lost; at [`MAX_LINK_RETRIES`] it is not recreated
    /// until one of its ports or our node changes.
    lost_count: HashMap<PlannedLink, u32>,
    /// Where the links' listeners report [`LinkLost`].
    lost_tx: pw::channel::Sender<LinkLost>,
    next_serial: u64,
}

impl LinkCapture {
    fn new(
        core: pw::core::CoreRc,
        filter: NodeFilter,
        lost_tx: pw::channel::Sender<LinkLost>,
    ) -> Self {
        Self {
            core,
            filter,
            own_node: None,
            links: HashMap::new(),
            lost_count: HashMap::new(),
            lost_tx,
            next_serial: 0,
        }
    }

    /// Asks the server for `key` and watches the new link for failure or removal.
    fn create_link(&mut self, link_factory: &str, key: PlannedLink) {
        let props = pw::properties::properties! {
            "link.output.node" => key.out_node.to_string(),
            "link.output.port" => key.out_port.to_string(),
            "link.input.node" => key.in_node.to_string(),
            "link.input.port" => key.in_port.to_string(),
            "object.linger" => "false",
        };
        let link = match self
            .core
            .create_object::<pw::link::Link>(link_factory, &props)
        {
            Ok(link) => link,
            Err(e) => {
                tracing::warn!(?key, error = %e, "cannot create PipeWire link");
                return;
            }
        };
        let serial = self.next_serial;
        self.next_serial += 1;
        let lost = LinkLost { key, serial };
        // The listeners only report: the entry is dropped later from the loop, never from
        // inside the link's own callbacks.
        let info_listener = link
            .add_listener_local()
            .info({
                let tx = self.lost_tx.clone();
                move |info| {
                    if let pw::link::LinkState::Error(message) = info.state() {
                        tracing::debug!(?key, message, "PipeWire link failed");
                        let _ = tx.send(lost);
                    }
                }
            })
            .register();
        let proxy_listener = link
            .upcast_ref()
            .add_listener_local()
            .removed({
                let tx = self.lost_tx.clone();
                move || {
                    let _ = tx.send(lost);
                }
            })
            .error({
                let tx = self.lost_tx.clone();
                move |_seq, res, message| {
                    tracing::debug!(?key, res, message, "PipeWire link error");
                    let _ = tx.send(lost);
                }
            })
            .register();
        tracing::debug!(?key, "linked application stream into capture");
        self.links.insert(
            key,
            OwnedLink {
                _info_listener: info_listener,
                _proxy_listener: proxy_listener,
                _link: link,
                serial,
            },
        );
    }
}

/// Registry mirror for one connection; with `capture` set it also maintains the links.
struct Tracker {
    registry: pw::registry::RegistryRc,
    model: GraphModel,
    procs: ProcFs,
    bound: HashMap<u32, Bound>,
    link_factory: String,
    capture: Option<LinkCapture>,
}

impl Tracker {
    fn new(registry: pw::registry::RegistryRc, capture: Option<LinkCapture>) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            registry,
            model: GraphModel::default(),
            procs: ProcFs::default(),
            bound: HashMap::new(),
            link_factory: "link-factory".to_owned(),
            capture,
        }))
    }

    /// Registers the registry listener that feeds `this`.
    fn watch(this: &Rc<RefCell<Self>>) -> Result<pw::registry::Listener> {
        let registry = this
            .try_borrow()
            .map_err(|_| CaptureError::Backend("PipeWire registry busy".to_owned()))?
            .registry
            .clone();
        let on_global = Rc::downgrade(this);
        let on_remove = Rc::downgrade(this);
        Ok(registry
            .add_listener_local()
            .global(move |global| {
                if let Some(this) = on_global.upgrade() {
                    Tracker::on_global(&this, global);
                }
            })
            .global_remove(move |id| {
                if let Some(this) = on_remove.upgrade() {
                    if let Ok(mut tracker) = this.try_borrow_mut() {
                        tracker.on_global_remove(id);
                    }
                }
            })
            .register())
    }

    fn on_global(
        this: &Rc<RefCell<Self>>,
        global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
    ) {
        let Some(props) = global.props else {
            return;
        };
        let Ok(mut tracker) = this.try_borrow_mut() else {
            return;
        };
        let id = global.id;
        match global.type_ {
            ObjectType::Client => {
                let mut client = ClientModel::default();
                client.apply_props(|k| props.get(k));
                tracker.model.clients.insert(id, client);
                match tracker.registry.bind::<pw::client::Client, _>(global) {
                    Ok(proxy) => {
                        let weak = Rc::downgrade(this);
                        let listener = proxy
                            .add_listener_local()
                            .info(move |info| {
                                let props = info.props();
                                with_tracker(&weak, |t| {
                                    if let Some(client) = t.model.clients.get_mut(&id) {
                                        if let Some(props) = props {
                                            client.apply_props(|k| props.get(k));
                                        }
                                        client.info_seen = true;
                                    }
                                    t.reconcile();
                                });
                            })
                            .register();
                        tracker.bound.insert(id, Bound::Client(listener, proxy));
                    }
                    Err(e) => {
                        tracing::debug!(id, error = %e, "cannot bind PipeWire client");
                        // The global props are all we will get.
                        if let Some(client) = tracker.model.clients.get_mut(&id) {
                            client.info_seen = true;
                        }
                    }
                }
            }
            ObjectType::Node if props.get("media.class") == Some(OUTPUT_STREAM_CLASS) => {
                let mut node = NodeModel::default();
                node.apply_props(|k| props.get(k));
                tracker.model.nodes.insert(id, node);
                match tracker.registry.bind::<pw::node::Node, _>(global) {
                    Ok(proxy) => {
                        let weak = Rc::downgrade(this);
                        let listener = proxy
                            .add_listener_local()
                            .info(move |info| {
                                let props = info.props();
                                with_tracker(&weak, |t| {
                                    if let Some(node) = t.model.nodes.get_mut(&id) {
                                        if let Some(props) = props {
                                            node.apply_props(|k| props.get(k));
                                        }
                                        node.info_seen = true;
                                    }
                                    t.reconcile();
                                });
                            })
                            .register();
                        tracker.bound.insert(id, Bound::Node(listener, proxy));
                    }
                    Err(e) => tracing::debug!(id, error = %e, "cannot bind PipeWire node"),
                }
            }
            ObjectType::Node
                if props
                    .get("media.class")
                    .is_some_and(|class| class.starts_with("Audio/Sink")) =>
            {
                let client = props.get("client.id").and_then(|v| v.trim().parse().ok());
                tracker.model.sinks.insert(id, client);
                tracker.reconcile();
            }
            ObjectType::Port => {
                if let Some(port) = PortModel::from_props(|k| props.get(k)) {
                    tracker.model.ports.insert(id, port);
                    tracker.reconcile();
                }
            }
            ObjectType::Factory => {
                if props.get("factory.type.name") == Some(ObjectType::Link.to_str()) {
                    if let Some(name) = props.get("factory.name") {
                        tracker.link_factory = name.to_owned();
                    }
                }
            }
            _ => {}
        }
    }

    fn on_global_remove(&mut self, id: u32) {
        self.model.clients.remove(&id);
        self.model.nodes.remove(&id);
        self.model.ports.remove(&id);
        self.model.sinks.remove(&id);
        self.bound.remove(&id);
        if let Some(capture) = self.capture.as_mut() {
            // The server already destroyed links touching a removed node/port; links to new
            // ports get a fresh retry budget.
            let touches = |link: &PlannedLink| {
                link.out_node == id || link.out_port == id || link.in_port == id
            };
            capture.links.retain(|link, _| !touches(link));
            capture.lost_count.retain(|link, _| !touches(link));
        }
        self.reconcile();
    }

    /// Our stream got (re)connected as node `node_id`.
    fn set_own_node(&mut self, node_id: u32) {
        if let Some(capture) = self.capture.as_mut() {
            if capture.own_node != Some(node_id) {
                capture.own_node = Some(node_id);
                capture.links.clear();
                capture.lost_count.clear();
            }
        }
        self.reconcile();
    }

    /// A link we created failed or was removed by someone else: forget it and relink,
    /// at most [`MAX_LINK_RETRIES`] times per link.
    fn on_link_lost(&mut self, lost: LinkLost) {
        let Some(capture) = self.capture.as_mut() else {
            return;
        };
        if capture.links.get(&lost.key).map(|l| l.serial) != Some(lost.serial) {
            return; // already gone, or a newer link of the same key
        }
        capture.links.remove(&lost.key);
        let count = capture.lost_count.entry(lost.key).or_insert(0);
        *count += 1;
        if *count >= MAX_LINK_RETRIES {
            tracing::warn!(
                key = ?lost.key,
                attempts = *count,
                "PipeWire keeps failing or removing a capture link; giving up on it"
            );
        } else {
            tracing::debug!(key = ?lost.key, attempts = *count, "PipeWire capture link lost; relinking");
        }
        self.reconcile();
    }

    /// Creates missing links and removes unwanted ones (linked capture only).
    fn reconcile(&mut self) {
        let Tracker {
            model,
            procs,
            capture,
            link_factory,
            ..
        } = self;
        let Some(capture) = capture.as_mut() else {
            return;
        };
        let Some(own_node) = capture.own_node else {
            return;
        };
        let planned = model.plan_links(own_node, capture.filter, procs);
        // Dropping an unwanted link's proxy destroys it (once); see `OwnedLink`.
        capture.links.retain(|key, _| planned.contains(key));
        for key in planned {
            let given_up = capture
                .lost_count
                .get(&key)
                .is_some_and(|&n| n >= MAX_LINK_RETRIES);
            if capture.links.contains_key(&key) || given_up {
                continue;
            }
            capture.create_link(link_factory, key);
        }
    }
}

/// Runs `f` on the tracker behind `weak` if it is alive and not borrowed.
fn with_tracker(weak: &Weak<RefCell<Tracker>>, f: impl FnOnce(&mut Tracker)) {
    if let Some(this) = weak.upgrade() {
        if let Ok(mut tracker) = this.try_borrow_mut() {
            f(&mut tracker);
        }
    }
}

fn list_apps_on(remote: Option<&str>) -> Result<Vec<CaptureApp>> {
    let conn = Connection::open(remote)?;
    let registry = conn
        .core
        .get_registry_rc()
        .map_err(|e| backend("get registry", e))?;
    let tracker = Tracker::new(registry, None);
    let _listener = Tracker::watch(&tracker)?;
    // First round trip: all globals (and our bind requests); second: the bound proxies' info.
    conn.roundtrip(ROUNDTRIP_TIMEOUT)?;
    conn.roundtrip(ROUNDTRIP_TIMEOUT)?;
    let tracker = tracker
        .try_borrow()
        .map_err(|_| CaptureError::Backend("PipeWire registry busy".to_owned()))?;
    Ok(tracker.model.apps(std::process::id(), &tracker.procs))
}

/// Serialises the `EnumFormat` pod offered by the capture stream.
fn format_pod_bytes(format: AudioFormat) -> Result<Vec<u8>> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(format.sample_rate);
    info.set_channels(u32::from(format.channels));
    let mut position = [0u32; spa::param::audio::MAX_CHANNELS];
    match format.channels {
        1 => position[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO,
        2 => {
            position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
            position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
        }
        _ => {} // unpositioned
    }
    info.set_position(position);
    let object = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map(|(cursor, _)| cursor.into_inner())
    .map_err(|e| backend("serialize format", format!("{e:?}")))
}

/// Parses a negotiated `Format` param. Only raw F32LE audio is accepted.
fn parse_format_pod(param: &Pod) -> Result<AudioFormat> {
    use spa::param::format::{MediaSubtype, MediaType};
    let (media_type, media_subtype) = spa::param::format_utils::parse_format(param)
        .map_err(|e| CaptureError::Format(format!("unparsable PipeWire format: {e}")))?;
    if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
        return Err(CaptureError::Format(
            "PipeWire negotiated a non-raw-audio format".to_owned(),
        ));
    }
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.parse(param)
        .map_err(|e| CaptureError::Format(format!("unparsable PipeWire audio format: {e}")))?;
    if info.format() != spa::param::audio::AudioFormat::F32LE {
        return Err(CaptureError::Format(format!(
            "PipeWire negotiated {:?} instead of F32LE",
            info.format()
        )));
    }
    audio_format_from(info.rate(), info.channels())
}

/// What a capture thread captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Monitor of the default sink (`stream.capture.sink = true`, auto-connected).
    Monitor,
    /// Unconnected stream fed by links from matching application streams.
    Linked(NodeFilter),
}

/// Messages to the capture thread.
enum Command {
    /// Start delivering into this sink.
    Start(PcmSink),
    /// Quit the loop and release everything.
    Stop,
}

type ReadySlot = Rc<RefCell<Option<SyncSender<Result<AudioFormat>>>>>;

/// Reports the open outcome once (later calls are ignored).
fn signal_ready(slot: &ReadySlot, outcome: Result<AudioFormat>) -> bool {
    let sender = slot.try_borrow_mut().ok().and_then(|mut s| s.take());
    match sender {
        Some(tx) => {
            let _ = tx.try_send(outcome);
            true
        }
        None => false,
    }
}

/// User data of the stream listener. Lives on the capture thread.
struct StreamData {
    /// Where captured audio goes; `None` until `start`.
    sink: Rc<RefCell<Option<PcmSink>>>,
    /// Channels of the buffers PipeWire delivers.
    in_channels: usize,
    /// Channels of the reported format (what the sink receives).
    out_channels: usize,
    /// The format reported to `CaptureSource::format`.
    reported: AudioFormat,
    /// Preallocated conversion buffer (the process callback never allocates).
    scratch: Box<[f32]>,
    ready: ReadySlot,
    tracker: Option<Weak<RefCell<Tracker>>>,
}

/// The `process` callback: real-time safe (no allocation, lock, log or syscall).
fn on_process(stream: &pw::stream::Stream, data: &mut StreamData) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let Ok(mut slot) = data.sink.try_borrow_mut() else {
        return;
    };
    let Some(sink) = slot.as_mut() else {
        return; // not started yet: drop the buffer
    };
    let Some(first) = buffer.datas_mut().first_mut() else {
        return;
    };
    let (offset, size) = {
        let chunk = first.chunk();
        (chunk.offset() as usize, chunk.size() as usize)
    };
    let Some(bytes) = first.data() else {
        return;
    };
    let start = offset.min(bytes.len());
    let end = start.saturating_add(size).min(bytes.len());
    convert_f32le(
        &bytes[start..end],
        data.in_channels,
        data.out_channels,
        &mut data.scratch,
        |block| {
            sink.push(block);
        },
    );
}

fn stream_props(mode: Mode) -> pw::properties::PropertiesBox {
    let mut props = pw::properties::properties! {
        "media.type" => "Audio",
        "media.category" => "Capture",
        "media.role" => "Music",
        "media.name" => "headphone-for-all capture",
        "node.name" => APP_NAME,
        "node.description" => "headphone-for-all capture",
        "application.name" => APP_NAME,
    };
    match mode {
        Mode::Monitor => props.insert("stream.capture.sink", "true"),
        Mode::Linked(_) => {
            props.insert("node.autoconnect", "false");
            props.insert("node.dont-reconnect", "true");
            // Keep being scheduled (silence) while no application stream is linked.
            props.insert("node.always-process", "true");
        }
    }
    props
}

/// Body of the capture thread. Returns when stopped or on a fatal error.
fn run_capture(
    mode: Mode,
    remote: Option<String>,
    commands: pw::channel::Receiver<Command>,
    ready: ReadySlot,
) -> Result<()> {
    let conn = Connection::open(remote.as_deref())?;

    let (lost_tx, lost_rx) = pw::channel::channel::<LinkLost>();
    let tracker = match mode {
        Mode::Monitor => None,
        Mode::Linked(filter) => {
            let registry = conn
                .core
                .get_registry_rc()
                .map_err(|e| backend("get registry", e))?;
            Some(Tracker::new(
                registry,
                Some(LinkCapture::new(conn.core.clone(), filter, lost_tx)),
            ))
        }
    };
    let _registry_listener = match &tracker {
        Some(tracker) => Some(Tracker::watch(tracker)?),
        None => None,
    };
    let _lost_links = lost_rx.attach(conn.mainloop.loop_(), {
        let weak = tracker.as_ref().map(Rc::downgrade);
        move |lost| {
            if let Some(weak) = &weak {
                with_tracker(weak, |t| t.on_link_lost(lost));
            }
        }
    });

    let sink_slot: Rc<RefCell<Option<PcmSink>>> = Rc::new(RefCell::new(None));
    let stream = pw::stream::StreamRc::new(conn.core.clone(), APP_NAME, stream_props(mode))
        .map_err(|e| backend("create stream", e))?;
    let channels = usize::from(REQUESTED_FORMAT.channels);
    let data = StreamData {
        sink: Rc::clone(&sink_slot),
        in_channels: channels,
        out_channels: channels,
        reported: REQUESTED_FORMAT,
        scratch: vec![0.0; SCRATCH_SAMPLES].into_boxed_slice(),
        ready: Rc::clone(&ready),
        tracker: tracker.as_ref().map(Rc::downgrade),
    };
    let _stream_listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(|stream, data, _old, new| match new {
            StreamState::Error(message) => {
                tracing::warn!(%message, "PipeWire capture stream error");
                signal_ready(
                    &data.ready,
                    Err(CaptureError::Backend(format!(
                        "PipeWire capture stream failed: {message}"
                    ))),
                );
            }
            StreamState::Paused | StreamState::Streaming => {
                if let Some(weak) = &data.tracker {
                    let node_id = stream.node_id();
                    with_tracker(weak, |t| t.set_own_node(node_id));
                }
            }
            StreamState::Unconnected | StreamState::Connecting => {}
        })
        .param_changed(|_stream, data, id, param| {
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Some(param) = param else {
                return;
            };
            match parse_format_pod(param) {
                Ok(format) => {
                    data.in_channels = usize::from(format.channels);
                    let pending = data
                        .ready
                        .try_borrow()
                        .map(|s| s.is_some())
                        .unwrap_or(false);
                    if pending {
                        data.reported = format;
                        data.out_channels = usize::from(format.channels);
                        signal_ready(&data.ready, Ok(format));
                        tracing::debug!(?format, "PipeWire capture format negotiated");
                    } else if format != data.reported {
                        tracing::warn!(
                            negotiated = ?format,
                            reported = ?data.reported,
                            "PipeWire renegotiated the capture format; channels are adapted, \
                             a sample-rate change is not"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "unusable PipeWire capture format");
                    signal_ready(&data.ready, Err(e));
                }
            }
        })
        .process(on_process)
        .register()
        .map_err(|e| backend("register stream listener", e))?;

    let pod = format_pod_bytes(REQUESTED_FORMAT)?;
    let pod = Pod::from_bytes(&pod)
        .ok_or_else(|| CaptureError::Backend("PipeWire: invalid format pod".to_owned()))?;
    let mut params = [pod];
    let flags = match mode {
        Mode::Monitor => StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        Mode::Linked(_) => StreamFlags::MAP_BUFFERS | StreamFlags::DONT_RECONNECT,
    };
    stream
        .connect(spa::utils::Direction::Input, None, flags, &mut params)
        .map_err(|e| backend("connect stream", e))?;

    let _commands = commands.attach(conn.mainloop.loop_(), {
        let mainloop = conn.mainloop.clone();
        let sink_slot = Rc::clone(&sink_slot);
        move |command| match command {
            Command::Start(sink) => {
                if let Ok(mut slot) = sink_slot.try_borrow_mut() {
                    *slot = Some(sink);
                }
            }
            Command::Stop => mainloop.quit(),
        }
    });
    let _core_listener = conn
        .core
        .add_listener_local()
        .error({
            let mainloop = conn.mainloop.clone();
            let ready = Rc::clone(&ready);
            move |id, _seq, res, message| {
                if id == pw::core::PW_ID_CORE && is_connection_error(res) {
                    tracing::warn!(res, message, "PipeWire connection lost; capture stops");
                    signal_ready(
                        &ready,
                        Err(CaptureError::Backend(format!(
                            "PipeWire connection failed: {message}"
                        ))),
                    );
                    mainloop.quit();
                } else {
                    tracing::debug!(id, res, message, "PipeWire object error");
                }
            }
        })
        .register();
    let grace = conn.mainloop.loop_().add_timer({
        let ready = Rc::clone(&ready);
        move |_| {
            if signal_ready(&ready, Ok(REQUESTED_FORMAT)) {
                tracing::debug!("no PipeWire format negotiated yet; reporting the requested one");
            }
        }
    });
    let _ = grace.update_timer(Some(NEGOTIATION_GRACE), None);

    conn.mainloop.run();

    if let Err(e) = stream.disconnect() {
        tracing::debug!(error = %e, "PipeWire stream disconnect failed");
    }
    Ok(())
}

/// A running PipeWire capture (see the module docs).
struct PipeWireCapture {
    description: String,
    format: AudioFormat,
    commands: pw::channel::Sender<Command>,
    thread: Option<JoinHandle<()>>,
    started: bool,
}

impl PipeWireCapture {
    /// Spawns the capture thread, connects the stream and waits for the negotiated format.
    fn open(mode: Mode, description: String, remote: Option<String>) -> Result<Self> {
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<AudioFormat>>(1);
        let (commands, command_rx) = pw::channel::channel::<Command>();
        let thread = std::thread::Builder::new()
            .name("hfa-pw-capture".to_owned())
            .spawn(move || {
                let ready: ReadySlot = Rc::new(RefCell::new(Some(ready_tx)));
                if let Err(e) = run_capture(mode, remote, command_rx, Rc::clone(&ready)) {
                    if !signal_ready(&ready, Err(e.clone())) {
                        tracing::warn!(error = %e, "PipeWire capture thread failed");
                    }
                }
            })
            .map_err(|e| backend("spawn capture thread", e))?;

        let mut capture = Self {
            description,
            format: REQUESTED_FORMAT,
            commands,
            thread: Some(thread),
            started: false,
        };
        match ready_rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(format)) => {
                capture.format = format;
                tracing::info!(source = %capture.description, ?format, "PipeWire capture opened");
                Ok(capture)
            }
            Ok(Err(e)) => Err(e), // `capture` drops here: stops and joins the thread
            Err(RecvTimeoutError::Timeout) => Err(CaptureError::Backend(
                "PipeWire capture stream did not start in time".to_owned(),
            )),
            Err(RecvTimeoutError::Disconnected) => Err(CaptureError::Backend(
                "PipeWire capture thread exited unexpectedly".to_owned(),
            )),
        }
    }
}

impl CaptureSource for PipeWireCapture {
    fn describe(&self) -> String {
        self.description.clone()
    }

    fn format(&self) -> AudioFormat {
        self.format
    }

    fn start(&mut self, sink: PcmSink) -> Result<()> {
        let alive = self.thread.as_ref().is_some_and(|t| !t.is_finished());
        if !alive {
            return Err(CaptureError::Backend(
                "PipeWire capture is stopped or lost its connection; open a new source".to_owned(),
            ));
        }
        if self.started {
            return Err(CaptureError::AlreadyRunning);
        }
        self.commands
            .send(Command::Start(sink))
            .map_err(|_| CaptureError::Backend("PipeWire capture thread is gone".to_owned()))?;
        self.started = true;
        Ok(())
    }

    fn error(&self) -> Option<String> {
        // The capture thread only ends on its own when it lost the PipeWire connection.
        (self.started && self.thread.as_ref().is_some_and(|t| t.is_finished()))
            .then(|| "the PipeWire capture lost its connection; start the sender again".to_owned())
    }

    fn stop(&mut self) {
        if let Some(thread) = self.thread.take() {
            // Fails only if the thread already exited (then there is nothing to stop).
            let _ = self.commands.send(Command::Stop);
            if thread.join().is_err() {
                tracing::warn!("PipeWire capture thread panicked");
            }
        }
    }
}

impl Drop for PipeWireCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_parents(pairs: &[(u32, u32)]) -> impl Fn(u32) -> Option<u32> + '_ {
        move |pid| pairs.iter().find(|(c, _)| *c == pid).map(|(_, p)| *p)
    }

    /// A fake process table: parent links and sandbox-to-host pid mappings.
    #[derive(Default)]
    struct FakeProcs {
        parents: Vec<(u32, u32)>,
        /// ((sandbox pid, app id), host pid)
        sandboxes: Vec<((u32, Option<&'static str>), u32)>,
    }

    impl Procs for FakeProcs {
        fn parent_of(&self, pid: u32) -> Option<u32> {
            self.parents
                .iter()
                .find(|(c, _)| *c == pid)
                .map(|(_, p)| *p)
        }

        fn host_pid(&self, sandbox_pid: u32, app_id: Option<&str>) -> Option<u32> {
            self.sandboxes
                .iter()
                .find(|((pid, id), _)| *pid == sandbox_pid && *id == app_id)
                .map(|(_, host)| *host)
        }
    }

    fn procs_with_parents(parents: &[(u32, u32)]) -> FakeProcs {
        FakeProcs {
            parents: parents.to_vec(),
            ..FakeProcs::default()
        }
    }

    #[test]
    fn pids_parse_strictly() {
        assert_eq!(parse_pid("1234"), Some(1234));
        assert_eq!(parse_pid(" 42 "), Some(42));
        assert_eq!(parse_pid("0"), None);
        assert_eq!(parse_pid("-3"), None);
        assert_eq!(parse_pid("12a"), None);
        assert_eq!(parse_pid(""), None);
    }

    #[test]
    fn ppid_parses_after_the_last_parenthesis() {
        assert_eq!(parse_ppid("123 (bash) S 99 123 123 0 -1"), Some(99));
        // `comm` may contain spaces and parentheses.
        assert_eq!(parse_ppid("77 (Web Content) (x)) R 12 1 1"), Some(12));
        assert_eq!(parse_ppid("garbage"), None);
        assert_eq!(parse_ppid("5 (a) S"), None);
        // Our own process has a real parent in /proc.
        assert!(read_ppid(std::process::id()).is_some());
    }

    #[test]
    fn descendant_walk_follows_parents_and_stops_on_cycles() {
        let parents = [(30, 20), (20, 10), (10, 1), (7, 7), (8, 9), (9, 8)];
        let parent_of = fake_parents(&parents);
        assert!(is_descendant(30, 10, &parent_of));
        assert!(is_descendant(20, 10, &parent_of));
        assert!(is_descendant(30, 1, &parent_of));
        assert!(!is_descendant(10, 30, &parent_of));
        assert!(!is_descendant(7, 10, &parent_of)); // self-parent
        assert!(!is_descendant(8, 10, &parent_of)); // 8 <-> 9 cycle terminates
        assert!(!is_descendant(99, 10, &parent_of)); // unknown process
    }

    #[test]
    fn filters_select_the_right_processes() {
        let procs = FakeProcs {
            parents: vec![(30, 20), (20, 10)],
            sandboxes: vec![((2, Some("org.example.App")), 30)],
        };
        let host = Owner::Host;
        let sandboxed = |app_id: Option<&str>| Owner::Sandboxed {
            pid: 2,
            app_id: app_id.map(str::to_owned),
        };
        let all_but_5 = NodeFilter::AllExcept(5);
        assert!(all_but_5.matches(&host(6), &procs));
        assert!(!all_but_5.matches(&host(5), &procs));
        assert!(!all_but_5.matches(&Owner::Pending, &procs));
        assert!(all_but_5.matches(&Owner::Unknown, &procs));
        // A sandboxed stream is another app even if its host pid is unknown ...
        assert!(all_but_5.matches(&sandboxed(None), &procs));
        // ... but it is excluded when it maps to the excluded pid.
        assert!(!NodeFilter::AllExcept(30).matches(&sandboxed(Some("org.example.App")), &procs));

        assert!(NodeFilter::ProcessTree(10).matches(&host(10), &procs));
        assert!(NodeFilter::ProcessTree(10).matches(&host(30), &procs));
        assert!(!NodeFilter::ProcessTree(20).matches(&host(10), &procs));
        assert!(!NodeFilter::ProcessTree(10).matches(&Owner::Unknown, &procs));
        assert!(!NodeFilter::ProcessTree(10).matches(&Owner::Pending, &procs));
        // Sandboxed streams match by their host pid, never by the sandbox-internal one.
        assert!(NodeFilter::ProcessTree(20).matches(&sandboxed(Some("org.example.App")), &procs));
        assert!(!NodeFilter::ProcessTree(2).matches(&sandboxed(Some("org.example.App")), &procs));
        assert!(!NodeFilter::ProcessTree(2).matches(&sandboxed(None), &procs));
    }

    #[test]
    fn core_errors_stop_the_capture_only_when_the_connection_broke() {
        assert!(is_connection_error(-libc::EPIPE));
        assert!(is_connection_error(-libc::ECONNRESET));
        assert!(is_connection_error(-libc::EPROTO));
        // "unknown resource N op:7": a destroy of an object the server already removed.
        assert!(!is_connection_error(-libc::ENOENT));
        assert!(!is_connection_error(-libc::EINVAL));
        assert!(!is_connection_error(0));
        assert!(!is_connection_error(i32::MIN));
    }

    #[test]
    fn channels_route_onto_a_stereo_capture() {
        let stereo: BTreeMap<String, u32> = [("FL".to_owned(), 101), ("FR".to_owned(), 102)].into();
        assert_eq!(route_channel("FL", &stereo), vec![101]);
        assert_eq!(route_channel("FR", &stereo), vec![102]);
        assert_eq!(route_channel("MONO", &stereo), vec![101, 102]);
        assert_eq!(route_channel("FC", &stereo), vec![101, 102]);
        assert_eq!(route_channel("RL", &stereo), vec![101]);
        assert_eq!(route_channel("SR", &stereo), vec![102]);
        assert_eq!(route_channel("AUX0", &stereo), vec![101]);
        assert_eq!(route_channel("AUX1", &stereo), vec![102]);
        assert!(route_channel("LFE", &stereo).is_empty());

        let mono: BTreeMap<String, u32> = [("MONO".to_owned(), 7)].into();
        assert_eq!(route_channel("FL", &mono), vec![7]);
        assert_eq!(route_channel("FR", &mono), vec![7]);
        assert!(route_channel("LFE", &mono).is_empty());
    }

    #[test]
    fn ports_parse_from_global_props() {
        let props: HashMap<&str, &str> = [
            ("node.id", "43"),
            ("port.direction", "out"),
            ("audio.channel", "FR"),
        ]
        .into();
        let port = PortModel::from_props(|k| props.get(k).copied()).expect("port");
        assert_eq!(
            port,
            PortModel {
                node_id: 43,
                output: true,
                monitor: false,
                channel: "FR".to_owned()
            }
        );

        let monitor: HashMap<&str, &str> = [
            ("node.id", "49"),
            ("port.direction", "out"),
            ("port.monitor", "true"),
        ]
        .into();
        let port = PortModel::from_props(|k| monitor.get(k).copied()).expect("port");
        assert!(port.monitor);
        assert_eq!(
            port.channel, "MONO",
            "missing audio.channel defaults to MONO"
        );

        let no_node: HashMap<&str, &str> = [("port.direction", "in")].into();
        assert!(PortModel::from_props(|k| no_node.get(k).copied()).is_none());
        let bad_dir: HashMap<&str, &str> = [("node.id", "1"), ("port.direction", "x")].into();
        assert!(PortModel::from_props(|k| bad_dir.get(k).copied()).is_none());
    }

    fn port(node_id: u32, output: bool, channel: &str) -> PortModel {
        PortModel {
            node_id,
            output,
            monitor: false,
            channel: channel.to_owned(),
        }
    }

    fn client(sec_pid: Option<u32>, app_pid: Option<u32>, name: &str) -> ClientModel {
        ClientModel {
            sec_pid,
            app_pid,
            app_name: Some(name.to_owned()),
            info_seen: true,
            ..ClientModel::default()
        }
    }

    fn pulse_client() -> ClientModel {
        ClientModel {
            api: Some("pipewire-pulse".into()),
            ..client(Some(900), Some(900), "pipewire-pulse")
        }
    }

    fn stream_node(client: u32, app_pid: Option<u32>, name: Option<&str>) -> NodeModel {
        NodeModel {
            client_id: Some(client),
            app_pid,
            app_name: name.map(str::to_owned),
            node_name: Some("node".into()),
            relay: false,
            info_seen: true,
        }
    }

    /// Our capture node 50 (FL/FR inputs + monitor ports), three players and a pulse client.
    fn sample_graph(own_pid: u32) -> GraphModel {
        let mut g = GraphModel::default();
        // Native clients: pid from sec.pid; client 3 is pipewire-pulse.
        g.clients.insert(1, client(Some(100), Some(100), "mpv"));
        g.clients.insert(2, client(Some(own_pid), None, "hfa"));
        g.clients.insert(3, pulse_client());
        g.nodes.insert(10, stream_node(1, None, Some("mpv"))); // pid 100 via client
        g.nodes.insert(11, stream_node(2, None, None)); // our own playback
        g.nodes
            .insert(12, stream_node(3, Some(300), Some("Firefox"))); // pulse: pid from node
        g.nodes.insert(13, stream_node(1, None, Some("mpv"))); // second mpv stream (mono)
        g.nodes.insert(
            14,
            NodeModel {
                info_seen: false,
                ..stream_node(1, None, Some("late"))
            },
        );
        // Ports.
        for (id, p) in [
            (500, port(50, false, "FL")),
            (501, port(50, false, "FR")),
            (
                502,
                PortModel {
                    monitor: true,
                    ..port(50, true, "FL")
                },
            ),
            (100, port(10, true, "FL")),
            (101, port(10, true, "FR")),
            (110, port(11, true, "FL")),
            (111, port(11, true, "FR")),
            (120, port(12, true, "FL")),
            (121, port(12, true, "FR")),
            (122, port(12, true, "LFE")),
            (130, port(13, true, "MONO")),
            (140, port(14, true, "FL")),
        ] {
            g.ports.insert(id, p);
        }
        g
    }

    fn link(out_node: u32, out_port: u32, in_port: u32) -> PlannedLink {
        PlannedLink {
            out_node,
            out_port,
            in_node: 50,
            in_port,
        }
    }

    #[test]
    fn node_owners_resolve_by_client_kind() {
        let own = 4242;
        let mut g = sample_graph(own);
        assert_eq!(g.node_owner(10), Owner::Host(100));
        assert_eq!(
            g.node_owner(11),
            Owner::Host(own),
            "native: pipewire.sec.pid"
        );
        assert_eq!(
            g.node_owner(12),
            Owner::Host(300),
            "pulse: the node's application.process.id, not the pulse server's sec.pid"
        );
        assert_eq!(
            g.node_owner(14),
            Owner::Pending,
            "before the node info arrives"
        );
        assert_eq!(g.node_owner(99), Owner::Pending);

        // A native client's own report (e.g. from a sandbox's pid namespace) never beats
        // the kernel-verified sec.pid.
        g.nodes.get_mut(&10).expect("node").app_pid = Some(2);
        g.clients.get_mut(&1).expect("client").app_pid = Some(2);
        assert_eq!(g.node_owner(10), Owner::Host(100));
        // Without a sec.pid the report is used.
        g.clients.get_mut(&1).expect("client").sec_pid = None;
        assert_eq!(g.node_owner(10), Owner::Host(2));

        // A pulse stream without any pid is unknown (the pulse server's pid is not the app).
        g.nodes.get_mut(&12).expect("node").app_pid = None;
        g.clients.get_mut(&3).expect("client").app_pid = None;
        assert_eq!(g.node_owner(12), Owner::Unknown);

        // Until the client info has arrived, a native-looking client may still turn out
        // to be pipewire-pulse: wait.
        g.clients.get_mut(&2).expect("client").info_seen = false;
        assert_eq!(g.node_owner(11), Owner::Pending);
        // A client that is not (yet) known at all: wait as well.
        g.nodes.get_mut(&13).expect("node").client_id = Some(77);
        assert_eq!(g.node_owner(13), Owner::Pending);
    }

    #[test]
    fn sandboxed_pulse_streams_report_their_namespace_pid_as_sandboxed() {
        let mut g = GraphModel::default();
        let flatpak_pulse = |app_id: &str| {
            let mut c = pulse_client();
            c.apply_props(|k| match k {
                "pipewire.access" => Some("flatpak"),
                "pipewire.access.portal.app_id" => Some(app_id),
                _ => None,
            });
            c
        };
        g.clients.insert(5, flatpak_pulse("org.mozilla.firefox"));
        g.clients.insert(6, flatpak_pulse("com.spotify.Client"));
        g.nodes.insert(20, stream_node(5, Some(2), Some("Firefox")));
        g.nodes.insert(21, stream_node(6, Some(2), Some("Spotify")));
        assert_eq!(
            g.node_owner(20),
            Owner::Sandboxed {
                pid: 2,
                app_id: Some("org.mozilla.firefox".into())
            }
        );
        for (id, p) in [
            (500, port(50, false, "FL")),
            (501, port(50, false, "FR")),
            (200, port(20, true, "FL")),
            (210, port(21, true, "FL")),
        ] {
            g.ports.insert(id, p);
        }

        // Only Firefox can be mapped to a host pid: only Firefox is listed, under its host
        // pid; Spotify is not listed under a made-up pid 2 (and never merged with Firefox).
        let procs = FakeProcs {
            parents: vec![(7001, 7000)],
            sandboxes: vec![((2, Some("org.mozilla.firefox")), 7001)],
        };
        assert_eq!(
            g.apps(4242, &procs),
            vec![CaptureApp {
                pid: 7001,
                name: "Firefox".into()
            }]
        );
        // system-excl still takes both (neither is this process).
        assert_eq!(
            g.plan_links(50, NodeFilter::AllExcept(4242), &procs),
            [link(20, 200, 500), link(21, 210, 500)].into()
        );
        // Per-process capture matches host pids (and their ancestors), not pid 2.
        assert_eq!(
            g.plan_links(50, NodeFilter::ProcessTree(7000), &procs),
            [link(20, 200, 500)].into()
        );
        assert!(g
            .plan_links(50, NodeFilter::ProcessTree(2), &procs)
            .is_empty());
    }

    #[test]
    fn client_and_node_props_are_merged() {
        let mut c = ClientModel::default();
        c.apply_props(|k| match k {
            "pipewire.sec.pid" => Some("900"),
            "pipewire.access" => Some("unrestricted"),
            _ => None,
        });
        assert_eq!(c.sec_pid, Some(900));
        assert!(!c.sandboxed && !c.is_pulse());
        c.apply_props(|k| match k {
            "client.api" => Some("pipewire-pulse"),
            "application.process.id" => Some("2"),
            "application.name" => Some("Firefox"),
            "pipewire.access.portal.app_id" => Some("org.mozilla.firefox"),
            _ => None,
        });
        assert!(c.is_pulse() && c.sandboxed);
        assert_eq!(c.sec_pid, Some(900), "absent keys keep their value");
        assert_eq!(c.app_pid, Some(2));
        assert_eq!(c.app_id.as_deref(), Some("org.mozilla.firefox"));

        let mut n = NodeModel::default();
        n.apply_props(|k| match k {
            "client.id" => Some("61"),
            "node.name" => Some("output.pw-loopback-1"),
            _ => None,
        });
        assert_eq!(n.client_id, Some(61));
        assert!(!n.relay);
        n.apply_props(|k| (k == "node.link-group").then_some("loopback-14593-18"));
        assert!(n.relay, "node.link-group marks a loopback/filter half");
        let mut v = NodeModel::default();
        v.apply_props(|k| (k == "node.virtual").then_some("true"));
        assert!(v.relay);
    }

    #[test]
    fn relay_streams_are_never_linked_or_listed() {
        let own = 4242;
        let mut g = sample_graph(own);
        // pw-loopback / module-loopback playback half (as seen on a live daemon).
        g.clients
            .insert(7, client(Some(14593), Some(14593), "pw-loopback"));
        let mut loopback = stream_node(7, None, None);
        loopback.apply_props(|k| match k {
            "node.link-group" => Some("loopback-14593-18"),
            "node.virtual" => Some("true"),
            _ => None,
        });
        g.nodes.insert(16, loopback);
        // An EasyEffects-style app: owns a virtual sink and plays its processed output.
        g.clients
            .insert(8, client(Some(800), Some(800), "easyeffects"));
        g.nodes
            .insert(17, stream_node(8, None, Some("easyeffects")));
        g.sinks.insert(60, Some(8));
        g.sinks.insert(38, None); // an ordinary sink
        for (id, p) in [
            (160, port(16, true, "FL")),
            (161, port(16, true, "FR")),
            (170, port(17, true, "FL")),
        ] {
            g.ports.insert(id, p);
        }
        assert!(g.is_relay(16) && g.is_relay(17));
        assert!(!g.is_relay(10) && !g.is_relay(12));

        let procs = FakeProcs::default();
        let planned = g.plan_links(50, NodeFilter::AllExcept(own), &procs);
        assert!(
            planned.iter().all(|l| l.out_node != 16 && l.out_node != 17),
            "{planned:?}"
        );
        assert!(g
            .plan_links(50, NodeFilter::ProcessTree(14593), &procs)
            .is_empty());
        let apps = g.apps(own, &procs);
        assert!(
            apps.iter().all(|a| a.pid != 14593 && a.pid != 800),
            "{apps:?}"
        );

        // Once the virtual sink is gone, EasyEffects' stream is an ordinary stream again.
        g.sinks.remove(&60);
        assert!(!g.is_relay(17));
    }

    #[test]
    fn exclude_self_links_every_other_stream() {
        let own = 4242;
        let g = sample_graph(own);
        let planned = g.plan_links(50, NodeFilter::AllExcept(own), &FakeProcs::default());
        let expected: BTreeSet<PlannedLink> = [
            link(10, 100, 500),
            link(10, 101, 501),
            link(12, 120, 500),
            link(12, 121, 501),
            link(13, 130, 500),
            link(13, 130, 501),
        ]
        .into();
        assert_eq!(planned, expected);
    }

    #[test]
    fn process_capture_links_the_process_tree_only() {
        let g = sample_graph(4242);
        // Firefox (300) is a child of 100.
        let procs = procs_with_parents(&[(300, 100), (100, 1)]);
        let planned = g.plan_links(50, NodeFilter::ProcessTree(100), &procs);
        let expected: BTreeSet<PlannedLink> = [
            link(10, 100, 500),
            link(10, 101, 501),
            link(12, 120, 500),
            link(12, 121, 501),
            link(13, 130, 500),
            link(13, 130, 501),
        ]
        .into();
        assert_eq!(planned, expected);

        let only_firefox = g.plan_links(50, NodeFilter::ProcessTree(300), &procs);
        assert_eq!(
            only_firefox,
            [link(12, 120, 500), link(12, 121, 501)].into()
        );
        // The pulse server's own pid is not an application.
        assert!(g
            .plan_links(50, NodeFilter::ProcessTree(900), &procs)
            .is_empty());
        // Without our input ports nothing can be linked.
        assert!(g
            .plan_links(77, NodeFilter::ProcessTree(300), &procs)
            .is_empty());
    }

    #[test]
    fn apps_are_deduplicated_sorted_and_exclude_self() {
        let own = 4242;
        let mut g = sample_graph(own);
        // A stream without any name falls back to "pid N".
        g.clients.insert(
            4,
            ClientModel {
                info_seen: true,
                ..ClientModel::default()
            },
        );
        g.nodes.insert(
            15,
            NodeModel {
                client_id: Some(4),
                app_pid: Some(555),
                info_seen: true,
                ..NodeModel::default()
            },
        );
        let apps = g.apps(own, &FakeProcs::default());
        assert_eq!(
            apps,
            vec![
                CaptureApp {
                    pid: 300,
                    name: "Firefox".into()
                },
                CaptureApp {
                    pid: 100,
                    name: "mpv".into()
                },
                CaptureApp {
                    pid: 555,
                    name: "pid 555".into()
                },
            ]
        );
    }

    #[test]
    fn proc_status_and_flatpak_metadata_parse() {
        let status = "Name:\tfirefox\nTgid:\t7001\nPid:\t7001\nPPid:\t7000\nNSpid:\t7001\t2\n";
        assert_eq!(parse_nspid(status), Some(vec![7001, 2]));
        assert_eq!(parse_nspid("NSpid:\t55\n"), Some(vec![55]));
        assert_eq!(parse_nspid("Name:\tx\n"), None);
        assert_eq!(parse_nspid("NSpid:\t55 x\n"), None);

        let info = "[Application]\nname=org.mozilla.firefox\nruntime=runtime/x\n\n\
                    [Instance]\ninstance-id=123\n";
        assert_eq!(parse_flatpak_info_app_id(info), Some("org.mozilla.firefox"));
        assert_eq!(
            parse_flatpak_info_app_id("[Runtime]\nname=org.gnome.Platform\n"),
            None
        );

        let cgroup = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/\
                      app-flatpak-org.mozilla.firefox-12345.scope\n";
        assert_eq!(
            parse_flatpak_cgroup_app_id(cgroup).as_deref(),
            Some("org.mozilla.firefox")
        );
        assert_eq!(
            parse_flatpak_cgroup_app_id("0::/app-flatpak-com.example.my\\x2dapp-9.scope\n")
                .as_deref(),
            Some("com.example.my-app")
        );
        assert_eq!(
            parse_flatpak_cgroup_app_id("0::/user.slice/session-2.scope\n"),
            None
        );
    }

    #[test]
    fn sandbox_pids_map_to_host_pids_only_when_unambiguous() {
        // Host pids 7001 (Firefox, sandbox pid 2), 8001 (Spotify, sandbox pid 2),
        // 8002 (Spotify, sandbox pid 3), 9000 (not sandboxed).
        let fake = |pid: u32, file: &str| -> Option<String> {
            let (nspid, app) = match pid {
                7001 => ("7001\t2", "org.mozilla.firefox"),
                8001 => ("8001\t2", "com.spotify.Client"),
                8002 => ("8002\t3", "com.spotify.Client"),
                9000 => ("9000", ""),
                _ => return None,
            };
            match file {
                "status" => Some(format!("Name:\tx\nNSpid:\t{nspid}\n")),
                "root/.flatpak-info" if pid != 8002 && !app.is_empty() => {
                    Some(format!("[Application]\nname={app}\n"))
                }
                // No access to the sandbox root: the cgroup scope still names the app.
                "cgroup" if !app.is_empty() => Some(format!("0::/app-flatpak-{app}-1.scope\n")),
                _ => None,
            }
        };
        let pids = [1, 7001, 8001, 8002, 9000];
        assert_eq!(
            find_host_pid(2, Some("org.mozilla.firefox"), pids, &fake),
            Some(7001)
        );
        assert_eq!(
            find_host_pid(2, Some("com.spotify.Client"), pids, &fake),
            Some(8001)
        );
        assert_eq!(
            find_host_pid(3, Some("com.spotify.Client"), pids, &fake),
            Some(8002),
            "app id from the cgroup when .flatpak-info is unreadable"
        );
        assert_eq!(find_host_pid(3, None, pids, &fake), Some(8002));
        assert_eq!(find_host_pid(2, None, pids, &fake), None, "ambiguous");
        assert_eq!(find_host_pid(2, Some("org.other.App"), pids, &fake), None);
        assert_eq!(
            find_host_pid(9000, None, pids, &fake),
            None,
            "not sandboxed"
        );
        // The real /proc: a non-sandboxed pid is never taken for a sandboxed one.
        let own = std::process::id();
        assert!(ProcFs::default().parent_of(own).is_some());
        assert!(!is_sandboxed_as(own, own, None, &read_proc_file));
    }

    fn le_bytes(samples: &[f32]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    fn convert_all(bytes: &[u8], in_ch: usize, out_ch: usize, scratch_len: usize) -> Vec<f32> {
        let mut scratch = vec![0.0; scratch_len];
        let mut out = Vec::new();
        let mut blocks = 0;
        convert_f32le(bytes, in_ch, out_ch, &mut scratch, |block| {
            assert_eq!(block.len() % out_ch.max(1), 0, "whole frames only");
            out.extend_from_slice(block);
            blocks += 1;
        });
        out
    }

    #[test]
    fn conversion_copies_matching_layouts_across_blocks() {
        let input: Vec<f32> = (0..20).map(|i| i as f32 * 0.01).collect();
        // Scratch of 6 samples = 3 stereo frames per block.
        assert_eq!(convert_all(&le_bytes(&input), 2, 2, 6), input);
        // A trailing partial frame (3 extra bytes) is ignored.
        let mut bytes = le_bytes(&input);
        bytes.extend_from_slice(&[1, 2, 3]);
        assert_eq!(convert_all(&bytes, 2, 2, 4096), input);
        assert!(convert_all(&[], 2, 2, 8).is_empty());
        // A scratch smaller than one frame emits nothing instead of looping forever.
        assert!(convert_all(&le_bytes(&input), 2, 2, 1).is_empty());
    }

    #[test]
    fn conversion_adapts_channel_counts() {
        let mono = [0.5f32, -0.25];
        assert_eq!(
            convert_all(&le_bytes(&mono), 1, 2, 64),
            vec![0.5, 0.5, -0.25, -0.25]
        );
        let stereo = [0.5f32, 0.25, -1.0, 1.0];
        assert_eq!(convert_all(&le_bytes(&stereo), 2, 1, 64), vec![0.375, 0.0]);
        let surround = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        assert_eq!(convert_all(&le_bytes(&surround), 6, 2, 64), vec![1.0, 2.0]);
        assert_eq!(
            convert_all(&le_bytes(&[1.0f32, 2.0]), 2, 3, 64),
            vec![1.0, 2.0, 0.0]
        );
    }

    #[test]
    fn negotiated_formats_are_validated() {
        assert_eq!(
            audio_format_from(48_000, 2).expect("valid"),
            AudioFormat::INTERNAL
        );
        assert_eq!(
            audio_format_from(44_100, 1).expect("valid"),
            AudioFormat::new(44_100, 1)
        );
        assert!(matches!(
            audio_format_from(0, 2),
            Err(CaptureError::Format(_))
        ));
        assert!(matches!(
            audio_format_from(48_000, 0),
            Err(CaptureError::Format(_))
        ));
        assert!(matches!(
            audio_format_from(48_000, 65),
            Err(CaptureError::Format(_))
        ));
        assert!(matches!(
            audio_format_from(48_000, 70_000),
            Err(CaptureError::Format(_))
        ));
    }

    #[test]
    fn offered_format_pod_round_trips() {
        for format in [AudioFormat::INTERNAL, AudioFormat::new(44_100, 1)] {
            let bytes = format_pod_bytes(format).expect("serialize");
            let pod = Pod::from_bytes(&bytes).expect("pod");
            assert_eq!(parse_format_pod(pod).expect("parse"), format);
            let mut info = spa::param::audio::AudioInfoRaw::new();
            info.parse(pod).expect("raw parse");
            assert_eq!(info.format(), spa::param::audio::AudioFormat::F32LE);
        }
        let stereo = format_pod_bytes(AudioFormat::INTERNAL).expect("serialize");
        let mut info = spa::param::audio::AudioInfoRaw::new();
        info.parse(Pod::from_bytes(&stereo).expect("pod"))
            .expect("parse");
        assert_eq!(
            &info.position()[..2],
            &[
                spa::sys::SPA_AUDIO_CHANNEL_FL,
                spa::sys::SPA_AUDIO_CHANNEL_FR
            ]
        );
    }

    #[test]
    fn a_missing_daemon_is_a_clear_error() {
        let remote = "hfa-test-no-such-pipewire-socket";
        let err = Connection::open(Some(remote)).err().expect("must fail");
        assert!(
            matches!(&err, CaptureError::Backend(m) if m.contains("cannot connect")),
            "{err}"
        );
        let err = PipeWireCapture::open(Mode::Monitor, "test".into(), Some(remote.into()))
            .err()
            .expect("must fail");
        assert!(matches!(err, CaptureError::Backend(_)), "{err}");
        assert!(list_apps_on(Some(remote)).is_err());
    }

    #[test]
    fn process_capture_rejects_missing_processes() {
        assert!(matches!(
            open_process(0).err(),
            Some(CaptureError::InvalidArgument(_))
        ));
        // PIDs are bounded by /proc/sys/kernel/pid_max (at most 2^22).
        assert!(matches!(
            open_process(u32::MAX - 1).err(),
            Some(CaptureError::NotFound(_))
        ));
    }

    #[test]
    fn capabilities_never_panic_and_are_consistent() {
        let caps = capabilities();
        assert!(!caps.mutes_local_output);
        assert_eq!(caps.system_mix, caps.per_app);
        assert!(caps.notes.contains("PipeWire"));
    }

    /// Live tests against a real PipeWire daemon (ignored by default).
    ///
    /// They need `pipewire` + `wireplumber` running for this user, a default sink named
    /// `hfa-test-sink` (the virtual-sink test switches the default away and back to it), the
    /// PipeWire tools `pw-play`, `pw-cli`, `pw-loopback` and `pw-metadata` on `PATH`, and a
    /// working `PcmSink` ring (`crate::ring`). In a headless container:
    ///
    /// ```sh
    /// export XDG_RUNTIME_DIR=/tmp/pw-run && mkdir -p -m 700 $XDG_RUNTIME_DIR
    /// dbus-run-session -- sh -c 'pipewire & sleep 1; wireplumber & sleep 1000000' &
    /// sleep 3
    /// pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=hfa-test-sink
    ///     media.class=Audio/Sink object.linger=true audio.position=[FL FR] }'
    /// cargo test -p hfa-capture -- --ignored --test-threads=1 live_
    /// ```
    mod live {
        use std::f32::consts::TAU;
        use std::path::Path;
        use std::process::{Child, Command as Process};
        use std::sync::Mutex;
        use std::time::Instant;

        use super::*;
        use crate::ring::pcm_ring;

        /// Live tests share one audio graph: run them one at a time.
        static SERIAL: Mutex<()> = Mutex::new(());

        const RATE: u32 = 48_000;

        /// Writes a 16-bit stereo 48 kHz sine WAV (amplitude 0.3).
        fn write_tone_wav(path: &Path, freq: f32, secs: u32) {
            let frames = RATE * secs;
            let data_len = frames * 4;
            let mut wav = Vec::with_capacity(44 + data_len as usize);
            wav.extend_from_slice(b"RIFF");
            wav.extend_from_slice(&(36 + data_len).to_le_bytes());
            wav.extend_from_slice(b"WAVEfmt ");
            wav.extend_from_slice(&16u32.to_le_bytes());
            wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
            wav.extend_from_slice(&2u16.to_le_bytes()); // channels
            wav.extend_from_slice(&RATE.to_le_bytes());
            wav.extend_from_slice(&(RATE * 4).to_le_bytes());
            wav.extend_from_slice(&4u16.to_le_bytes());
            wav.extend_from_slice(&16u16.to_le_bytes());
            wav.extend_from_slice(b"data");
            wav.extend_from_slice(&data_len.to_le_bytes());
            for i in 0..frames {
                let v = (0.3 * (TAU * freq * i as f32 / RATE as f32).sin() * 32767.0) as i16;
                wav.extend_from_slice(&v.to_le_bytes());
                wav.extend_from_slice(&v.to_le_bytes());
            }
            std::fs::write(path, wav).expect("write wav");
        }

        /// A `pw-play` child playing a tone; killed on drop.
        struct Player {
            child: Child,
            _dir: tempfile::TempDir,
        }

        impl Player {
            fn start(freq: f32) -> Self {
                let dir = tempfile::tempdir().expect("tempdir");
                let path = dir.path().join("tone.wav");
                write_tone_wav(&path, freq, 20);
                let child = Process::new("pw-play")
                    .arg(&path)
                    .spawn()
                    .expect("spawn pw-play (install pipewire-bin)");
                Self { child, _dir: dir }
            }

            fn pid(&self) -> u32 {
                self.child.id()
            }
        }

        impl Drop for Player {
            fn drop(&mut self) {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }

        /// A playback stream of *this* process (a hub playing its mix), on its own thread.
        struct SelfTone {
            quit: pw::channel::Sender<()>,
            thread: Option<JoinHandle<()>>,
        }

        impl SelfTone {
            fn start(freq: f32) -> Self {
                let (quit, quit_rx) = pw::channel::channel::<()>();
                let thread = std::thread::spawn(move || {
                    let conn = Connection::open(None).expect("connect");
                    let props = pw::properties::properties! {
                        "media.type" => "Audio",
                        "media.category" => "Playback",
                        "media.role" => "Music",
                        "node.name" => "hfa-test-self-tone",
                    };
                    let stream = pw::stream::StreamRc::new(conn.core.clone(), "self-tone", props)
                        .expect("stream");
                    let step = TAU * freq / RATE as f32;
                    let _listener = stream
                        .add_local_listener_with_user_data(0.0f32)
                        .process(move |stream, phase| {
                            let Some(mut buffer) = stream.dequeue_buffer() else {
                                return;
                            };
                            let Some(data) = buffer.datas_mut().first_mut() else {
                                return;
                            };
                            let mut written = 0;
                            if let Some(bytes) = data.data() {
                                let frames = bytes.len() / 8;
                                for frame in bytes.chunks_exact_mut(8).take(frames) {
                                    let v = (0.3 * phase.sin()).to_le_bytes();
                                    frame[..4].copy_from_slice(&v);
                                    frame[4..].copy_from_slice(&v);
                                    *phase = (*phase + step) % TAU;
                                }
                                written = frames * 8;
                            }
                            let chunk = data.chunk_mut();
                            *chunk.offset_mut() = 0;
                            *chunk.stride_mut() = 8;
                            *chunk.size_mut() = written as u32;
                        })
                        .register()
                        .expect("listener");
                    let pod = format_pod_bytes(AudioFormat::INTERNAL).expect("pod");
                    let mut params = [Pod::from_bytes(&pod).expect("pod")];
                    stream
                        .connect(
                            spa::utils::Direction::Output,
                            None,
                            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
                            &mut params,
                        )
                        .expect("connect");
                    let mainloop = conn.mainloop.clone();
                    let _quit = quit_rx.attach(conn.mainloop.loop_(), move |()| mainloop.quit());
                    conn.mainloop.run();
                });
                Self {
                    quit,
                    thread: Some(thread),
                }
            }
        }

        impl Drop for SelfTone {
            fn drop(&mut self) {
                let _ = self.quit.send(());
                if let Some(t) = self.thread.take() {
                    let _ = t.join();
                }
            }
        }

        /// Amplitude of the `freq` component of `mono` (Goertzel), ~0.3 for our tones.
        fn tone_amplitude(mono: &[f32], freq: f32) -> f32 {
            let n = mono.len() as f32;
            let coeff = 2.0 * (TAU * freq / RATE as f32).cos();
            let (mut s1, mut s2) = (0.0f32, 0.0f32);
            for &x in mono {
                let s = x + coeff * s1 - s2;
                s2 = s1;
                s1 = s;
            }
            let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
            2.0 * power.max(0.0).sqrt() / n
        }

        /// Starts `source`, captures for `secs`, stops, returns the left channel of the last
        /// half second.
        fn capture(mut source: Box<dyn CaptureSource>, secs: f32) -> Vec<f32> {
            let format = source.format();
            assert_eq!(format, AudioFormat::INTERNAL, "negotiated format");
            let (sink, mut ring) = pcm_ring(RATE as usize * 2 * 10);
            source.start(sink).expect("start");
            assert!(matches!(
                source.start(pcm_ring(16).0),
                Err(CaptureError::AlreadyRunning)
            ));
            let t0 = Instant::now();
            std::thread::sleep(Duration::from_secs_f32(secs));
            source.stop();
            source.stop(); // idempotent
            let mut all = vec![0.0; ring.available()];
            ring.pull(&mut all);
            let frames = all.len() / 2;
            let elapsed = t0.elapsed().as_secs_f32();
            assert!(
                frames as f32 > 0.5 * elapsed * RATE as f32,
                "captured only {frames} frames in {elapsed:.2} s"
            );
            let left: Vec<f32> = all.chunks_exact(2).map(|f| f[0]).collect();
            left[left.len().saturating_sub(RATE as usize / 2)..].to_vec()
        }

        fn serial() -> std::sync::MutexGuard<'static, ()> {
            SERIAL.lock().unwrap_or_else(|e| e.into_inner())
        }

        #[test]
        #[ignore = "needs a running PipeWire daemon (see module docs)"]
        fn live_system_mix_captures_everything() {
            let _serial = serial();
            let _other = Player::start(440.0);
            let _own = SelfTone::start(1000.0);
            std::thread::sleep(Duration::from_millis(500));
            let source = open_system(false).expect("open system");
            assert!(source.describe().contains("monitor"));
            let left = capture(source, 1.5);
            let (a440, a1000) = (tone_amplitude(&left, 440.0), tone_amplitude(&left, 1000.0));
            assert!(a440 > 0.1, "other app missing from the monitor: {a440}");
            assert!(
                a1000 > 0.1,
                "own playback missing from the monitor: {a1000}"
            );
        }

        #[test]
        #[ignore = "needs a running PipeWire daemon (see module docs)"]
        fn live_system_mix_excluding_self_drops_own_playback() {
            let _serial = serial();
            let _other = Player::start(440.0);
            let _own = SelfTone::start(1000.0);
            std::thread::sleep(Duration::from_millis(500));
            let source = open_system(true).expect("open system-excl");
            let left = capture(source, 1.5);
            let (a440, a1000) = (tone_amplitude(&left, 440.0), tone_amplitude(&left, 1000.0));
            assert!(a440 > 0.1, "other app not captured: {a440}");
            assert!(
                a1000 < 0.01,
                "own playback leaked into the capture: {a1000}"
            );
        }

        #[test]
        #[ignore = "needs a running PipeWire daemon (see module docs)"]
        fn live_process_capture_takes_only_that_process() {
            let _serial = serial();
            let wanted = Player::start(440.0);
            let _other = Player::start(660.0);
            std::thread::sleep(Duration::from_millis(500));

            let apps = list_apps().expect("list apps");
            let entry = apps.iter().find(|a| a.pid == wanted.pid());
            assert_eq!(
                entry.map(|a| a.name.as_str()),
                Some("pw-play"),
                "apps: {apps:?}"
            );
            assert!(apps.iter().all(|a| a.pid != std::process::id()));

            let source = open_process(wanted.pid()).expect("open process");
            let left = capture(source, 1.5);
            let (a440, a660) = (tone_amplitude(&left, 440.0), tone_amplitude(&left, 660.0));
            assert!(a440 > 0.1, "target process not captured: {a440}");
            assert!(
                a660 < 0.01,
                "another process leaked into the capture: {a660}"
            );
        }

        /// Parses `pw-cli ls <Type>` output into `(id, props)` blocks.
        fn pw_objects(kind: &str) -> Vec<(u32, HashMap<String, String>)> {
            let out = Process::new("pw-cli")
                .args(["ls", kind])
                .output()
                .expect("run pw-cli");
            let mut objects: Vec<(u32, HashMap<String, String>)> = Vec::new();
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("id ") {
                    if let Some(id) = rest.split(',').next().and_then(|i| i.parse().ok()) {
                        objects.push((id, HashMap::new()));
                    }
                } else if let (Some((key, value)), Some((_, props))) =
                    (line.split_once(" = "), objects.last_mut())
                {
                    props.insert(key.to_owned(), value.trim_matches('"').to_owned());
                }
            }
            objects
        }

        /// Sets the configured default sink by node name.
        fn set_default_sink(name: &str) {
            let status = Process::new("pw-metadata")
                .args([
                    "0",
                    "default.configured.audio.sink",
                    &format!("{{ \"name\": \"{name}\" }}"),
                    "Spa:String:JSON",
                ])
                .stdout(std::process::Stdio::null())
                .status()
                .expect("run pw-metadata");
            assert!(status.success());
        }

        /// A `pw-loopback` virtual sink (`hfa-vsink`) feeding `hfa-test-sink`, made the
        /// default sink (the EQ / filter-chain / virtual-sink setup). Restores the default
        /// sink and stops the loopback on drop.
        struct VirtualSink {
            child: Child,
        }

        impl VirtualSink {
            fn start() -> Self {
                let child = Process::new("pw-loopback")
                    .args([
                        "--capture-props=media.class=Audio/Sink node.name=hfa-vsink",
                        "--playback-props=target.object=hfa-test-sink",
                    ])
                    .spawn()
                    .expect("spawn pw-loopback");
                let sink = Self { child };
                let t0 = Instant::now();
                while !pw_objects("Node")
                    .iter()
                    .any(|(_, p)| p.get("node.name").map(String::as_str) == Some("hfa-vsink"))
                {
                    assert!(t0.elapsed() < Duration::from_secs(5), "no hfa-vsink");
                    std::thread::sleep(Duration::from_millis(50));
                }
                set_default_sink("hfa-vsink");
                std::thread::sleep(Duration::from_millis(300));
                sink
            }
        }

        impl Drop for VirtualSink {
            fn drop(&mut self) {
                set_default_sink("hfa-test-sink");
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }

        #[test]
        #[ignore = "needs a running PipeWire daemon (see module docs)"]
        fn live_system_mix_excluding_self_ignores_virtual_sink_relays() {
            let _serial = serial();
            let _vsink = VirtualSink::start();
            // Both play into the virtual sink; its loopback output stream relays the mix.
            let _other = Player::start(440.0);
            let _own = SelfTone::start(1000.0);
            std::thread::sleep(Duration::from_millis(500));
            let source = open_system(true).expect("open system-excl");
            let left = capture(source, 1.5);
            let (a440, a1000) = (tone_amplitude(&left, 440.0), tone_amplitude(&left, 1000.0));
            assert!(
                (0.2..0.4).contains(&a440),
                "other app must be captured exactly once (~0.3): {a440}"
            );
            assert!(
                a1000 < 0.01,
                "own playback came back through the virtual sink: {a1000}"
            );
        }

        /// Pulls everything captured so far (interleaved stereo).
        fn drain(ring: &mut crate::ring::PcmSource) -> Vec<f32> {
            let mut all = vec![0.0; ring.available()];
            let n = ring.pull(&mut all);
            all.truncate(n);
            all
        }

        fn left_tail(all: &[f32]) -> Vec<f32> {
            let left: Vec<f32> = all.chunks_exact(2).map(|f| f[0]).collect();
            left[left.len().saturating_sub(RATE as usize / 2)..].to_vec()
        }

        #[test]
        #[ignore = "needs a running PipeWire daemon (see module docs)"]
        fn live_links_that_become_unwanted_are_removed_and_capture_continues() {
            let _serial = serial();
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("tone.wav");
            write_tone_wav(&path, 440.0, 20);
            // A wrapper shell whose child plays; the child is linked as a descendant.
            let mut wrapper = Process::new("sh")
                .args(["-c", "pw-play \"$0\" & echo $!; wait"])
                .arg(&path)
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("spawn sh");
            let mut line = String::new();
            std::io::BufRead::read_line(
                &mut std::io::BufReader::new(wrapper.stdout.take().expect("stdout")),
                &mut line,
            )
            .expect("read child pid");
            let player: u32 = line.trim().parse().expect("child pid");
            std::thread::sleep(Duration::from_millis(500));

            let mut source = open_process(wrapper.id()).expect("open process");
            let (sink, mut ring) = pcm_ring(RATE as usize * 2 * 10);
            source.start(sink).expect("start");
            std::thread::sleep(Duration::from_millis(1000));
            let before = drain(&mut ring);
            assert!(
                tone_amplitude(&left_tail(&before), 440.0) > 0.1,
                "child of the wrapper not captured"
            );

            // The wrapper exits: the player is reparented and no longer wanted. Another
            // stream appearing makes the backend re-plan and remove the stale link.
            let _ = wrapper.kill();
            let _ = wrapper.wait();
            let _other = Player::start(660.0);
            let t0 = Instant::now();
            std::thread::sleep(Duration::from_millis(1500));
            let after = drain(&mut ring);
            let elapsed = t0.elapsed().as_secs_f32();
            source.stop();
            // Clean up the orphaned player.
            let _ = Process::new("kill")
                .args(["-9", &player.to_string()])
                .status();

            let frames = after.len() / 2;
            assert!(
                frames as f32 > 0.6 * elapsed * RATE as f32,
                "capture stalled after removing a link: {frames} frames in {elapsed:.2} s"
            );
            let tail = left_tail(&after);
            let (a440, a660) = (tone_amplitude(&tail, 440.0), tone_amplitude(&tail, 660.0));
            assert!(a440 < 0.01, "unwanted stream still linked: {a440}");
            assert!(a660 < 0.01, "unrelated stream linked: {a660}");
        }

        #[test]
        #[ignore = "needs a running PipeWire daemon (see module docs)"]
        fn live_links_removed_by_someone_else_are_recreated() {
            let _serial = serial();
            let wanted = Player::start(440.0);
            std::thread::sleep(Duration::from_millis(500));
            let mut source = open_process(wanted.pid()).expect("open process");
            let (sink, mut ring) = pcm_ring(RATE as usize * 2 * 10);
            source.start(sink).expect("start");
            std::thread::sleep(Duration::from_millis(1000));
            assert!(tone_amplitude(&left_tail(&drain(&mut ring)), 440.0) > 0.1);

            // Delete our links the way a patchbay (qpwgraph, Helvum, pw-link -d) would.
            let own_nodes: Vec<String> = pw_objects("Node")
                .into_iter()
                .filter(|(_, p)| p.get("node.name").map(String::as_str) == Some(APP_NAME))
                .map(|(id, _)| id.to_string())
                .collect();
            let ours: Vec<u32> = pw_objects("Link")
                .into_iter()
                .filter(|(_, p)| {
                    p.get("link.input.node")
                        .is_some_and(|n| own_nodes.contains(n))
                })
                .map(|(id, _)| id)
                .collect();
            assert!(!ours.is_empty(), "no links into {own_nodes:?}");
            for id in ours {
                let status = Process::new("pw-cli")
                    .args(["destroy", &id.to_string()])
                    .stdout(std::process::Stdio::null())
                    .status()
                    .expect("pw-cli destroy");
                assert!(status.success());
            }
            std::thread::sleep(Duration::from_millis(1500));
            let after = drain(&mut ring);
            source.stop();
            let a440 = tone_amplitude(&left_tail(&after), 440.0);
            assert!(a440 > 0.1, "externally removed link not recreated: {a440}");
        }

        #[test]
        #[ignore = "needs a running PipeWire daemon (see module docs)"]
        fn live_process_capture_picks_up_streams_that_start_later() {
            let _serial = serial();
            // Capture our own process (whose stream does not exist yet), then start playing.
            let mut source = open_process(std::process::id()).expect("open process");
            let (sink, mut ring) = pcm_ring(RATE as usize * 2 * 10);
            source.start(sink).expect("start");
            let _own = SelfTone::start(1000.0);
            std::thread::sleep(Duration::from_millis(1500));
            source.stop();
            let mut all = vec![0.0; ring.available()];
            ring.pull(&mut all);
            let left: Vec<f32> = all.chunks_exact(2).map(|f| f[0]).collect();
            // `node.always-process`: silence keeps flowing while nothing is linked yet.
            assert!(left.len() > RATE as usize, "only {} frames", left.len());
            let tail = &left[left.len().saturating_sub(RATE as usize / 2)..];
            assert!(
                tone_amplitude(tail, 1000.0) > 0.1,
                "late stream not linked ({} frames captured)",
                left.len()
            );
        }
    }
}
