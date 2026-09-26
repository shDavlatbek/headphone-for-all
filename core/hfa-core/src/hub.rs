//! The hub engine: accepts senders, receives their media, and mixes everything into one
//! output.
//!
//! # Structure
//!
//! - **Sockets:** TCP (control) and UDP (media) are bound on the same port number
//!   (`settings.port`; `0` picks a free number for both, see [`HubHandle::local_port`]), as
//!   dual-stack IPv6 sockets (`[::]`, `IPV6_V6ONLY` off: IPv6 and IPv4 peers), or on IPv4
//!   `0.0.0.0` only when the host has no IPv6. A port that is already taken is an error
//!   ([`crate::CoreError::Io`] naming the port).
//! - **Accept loop** (tokio): one task per sender connection ([`ControlChannel::accept`] with
//!   the hub's [`PairingManager`]; [`HubEvent::PairingCompleted`] /
//!   [`HubEvent::PairingFailed`]). Unauthenticated connections are limited, so nobody on the
//!   LAN can lock senders out: a peer must send its first handshake message within
//!   [`FIRST_MESSAGE_TIMEOUT`]; at most [`MAX_PENDING_PER_IP`] handshakes per IP address run
//!   at once (more are closed); when [`MAX_PENDING_HANDSHAKES`] are running, the oldest is
//!   dropped for a newcomer. Authenticated connections (at most [`MAX_CONNECTIONS`]) do not
//!   count against that budget; one that owns no stream for [`NO_STREAM_TIMEOUT`], or that
//!   sent nothing (senders ping every second) for [`CONTROL_IDLE_TIMEOUT`], is closed.
//!   Then:
//!   - `StreamStart` is validated (48 kHz, 2 channels, `frame_ms` 10 or 20, a 32-byte key, at
//!     most [`MAX_STREAMS`] streams, a stream id not in use) and registered with the
//!     [`MediaDemux`], the mixer and the source list, then answered with the stream's
//!     controls (`SetVolume`, `SetMute`, `SetPriority`: defaults, or the ones remembered for
//!     the device) followed by `StreamAccepted{udp_port}`, else `StreamRejected{reason}`.
//!   - `StreamStop`, `Bye` or a lost connection remove the connection's streams
//!     ([`HubEvent::SourceRemoved`]); `Ping` is answered with `Pong`.
//!   - The hub's own controls ([`HubHandle::set_gain`], ...) are forwarded to the sender as
//!     `SetVolume` / `SetMute` / `SetPriority` so its UI can show them. Controls are
//!     remembered per device id and re-applied when the device reconnects, also after a hub
//!     restart: they are saved in `<data_dir>/`[`HUB_CONTROLS_FILE`] (at most
//!     [`PREFS_SAVE_DELAY`] after a change, and when the hub stops) and loaded on start;
//!     devices that are no longer trusted are left out.
//! - **Receive thread** (`hfa-media-rx`, soft real-time like the mixer: arrival times feed
//!   the jitter estimate, and a starved tokio worker would let every jitter buffer run dry at
//!   once): UDP (a 64 KiB buffer, so an oversize datagram cannot make the receive fail on
//!   Windows; per-datagram errors never pause the loop; it checks for shutdown every
//!   [`RECEIVE_POLL`]) →
//!   [`MediaDemux::open`] (authentication, replay window) →
//!   the stream's jitter buffer (see `hub_mixer.rs`: DTX keep-alives only refresh the
//!   stream's activity; a `FLAG_RESET` packet, or the first audio after keep-alives, restarts
//!   the stream's playout).
//! - **Mixer thread** (`hfa-mixer`, soft real-time, see `hub_mixer.rs`): paced by the output
//!   ring fill; per stream: pop → decode / redundancy / FEC / PLC → drift-corrected resampler
//!   → mixer (gain, mute, priority ducking, limiter) → output format → output ring. It polls
//!   [`AudioOutput::has_error`] and restarts a failed output.
//! - **Stats task:** every [`STATS_INTERVAL`] it refreshes each [`SourceInfo`]
//!   ([`HubEvent::SourceUpdated`]) and sends `Stats` to its sender whenever a loss measurement
//!   completed (at least 40 frames, or none at all during DTX; a partial interval is extended
//!   and not reported). `Stats.loss_pct` is the **network** loss (before recovery, what the
//!   sender adapts to); [`StreamStats::loss_pct`] is what the listener misses after
//!   redundancy/FEC. A stream without datagrams (audio or DTX keep-alives) for [`IDLE_AFTER`]
//!   is inactive and reported to its sender as [`NO_MEDIA_LOSS_PCT`] loss (nothing arrives);
//!   after [`REMOVE_AFTER`] it is removed and its connection closed. So is the stream of a
//!   device that is no longer trusted (the store is shared with the rest of the process and
//!   re-read from disk every 5 s, see [`TrustStore`]).
//! - **Master gain:** the mixer starts with [`Settings::master_gain`] (persisted by the
//!   caller, e.g. the app saves it whenever the master slider moves);
//!   [`HubHandle::set_master_gain`] changes it while the hub runs.
//! - [`HubHandle::stop`] closes every connection with `Bye`, stops advertising, the tasks,
//!   the mixer and the output.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hfa_audio::mixer::MAX_GAIN;
use hfa_audio::{JitterBuffer, JitterConfig};
use hfa_capture::AudioOutput;
use hfa_proto::control::{
    Body, Pong, SetMute, SetPriority, SetVolume, Stats, StreamAccepted, StreamRejected, StreamStart,
};
use hfa_proto::{ControlMessage, MediaKey};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::{AbortHandle, JoinHandle, JoinSet};

use crate::config::{Settings, FRAME_MS_CHOICES};
use crate::control::{ControlChannel, PeerInfo};
use crate::discovery::Advertiser;
use crate::hub_mixer::{Controls, MixStream, MixerCommand, StreamShared};
pub use crate::hub_prefs::HUB_CONTROLS_FILE;
use crate::identity::{Identity, PeerRole, TrustStore};
use crate::media::MediaDemux;
use crate::pairing::{PairingInfo, PairingManager, DEFAULT_PAIRING_TTL};
use crate::sender::{load_identity, wait_stop};
use crate::{CoreError, Result};

/// Interval between `Stats` messages to each sender.
pub const STATS_INTERVAL: Duration = Duration::from_secs(1);
/// A stream without packets for this long is shown as inactive.
pub const IDLE_AFTER: Duration = Duration::from_secs(2);
/// A stream without packets for this long is removed.
pub const REMOVE_AFTER: Duration = Duration::from_secs(30);
/// `Stats.loss_pct` sent for a stream that received no datagram at all (not even a DTX
/// keep-alive) for [`IDLE_AFTER`]: everything is lost on the way (e.g. a firewall that lets
/// the TCP control connection through but blocks UDP).
pub const NO_MEDIA_LOSS_PCT: f32 = 100.0;
/// Most streams the hub mixes at once.
pub const MAX_STREAMS: usize = 16;
/// Most simultaneous authenticated control connections.
pub const MAX_CONNECTIONS: usize = 64;
/// Most connections still in the handshake (not yet authenticated). When a new one arrives
/// while this many are pending, the oldest pending one is dropped.
pub const MAX_PENDING_HANDSHAKES: usize = 16;
/// Most connections still in the handshake from one IP address (more are refused).
pub const MAX_PENDING_PER_IP: usize = 4;
/// How long a new connection may stay silent before the peer's first handshake message.
pub const FIRST_MESSAGE_TIMEOUT: Duration = Duration::from_secs(5);
/// An authenticated connection without a stream for this long is closed.
pub const NO_STREAM_TIMEOUT: Duration = Duration::from_secs(30);
/// An authenticated connection that sent nothing (senders `Ping` every second) for this long
/// is closed: its peer is gone, e.g. it changed networks and left a half-open connection.
pub const CONTROL_IDLE_TIMEOUT: Duration = Duration::from_secs(15);
/// How long after a control change the remembered controls are saved (changes in between,
/// e.g. a slider drag, are saved together).
pub const PREFS_SAVE_DELAY: Duration = Duration::from_millis(500);
/// Jitter-buffer target before the first jitter estimate (clamped to the settings' bounds).
const INITIAL_JITTER_TARGET_MS: u32 = 40;
/// Capacity of the event channel.
const EVENT_CAPACITY: usize = 256;
/// Fewest frames a loss measurement covers (a shorter interval is extended).
const MIN_FRAMES_FOR_LOSS: u64 = 40;
/// Every this many stats ticks the trust store is re-read from disk (5 s).
const TRUST_RELOAD_TICKS: u32 = 5;
/// How often a free TCP port is tried when `settings.port` is 0 and its UDP twin is taken.
const PORT_ATTEMPTS: usize = 16;
/// How long [`HubHandle::stop`] waits for the connections to say `Bye`.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
/// UDP receive buffer: larger than any UDP datagram.
const RECV_BUFFER: usize = 65_536;
/// Consecutive UDP receive errors after which the receive thread pauses 10 ms.
const ERRORS_BEFORE_BACKOFF: u32 = 100;
/// Longest the receive thread blocks in `recv_from` before it checks for shutdown.
const RECEIVE_POLL: Duration = Duration::from_millis(50);
/// Period the receive thread is promoted for ([`hfa_capture::rt::promote_current_thread`]):
/// the shortest frame a sender sends.
const RECEIVE_PERIOD: Duration = Duration::from_millis(10);

/// Everything needed to start a hub.
pub struct HubConfig {
    /// Settings (port, jitter bounds, data dir...).
    pub settings: Settings,
    /// The output (not yet started; the engine starts and stops it).
    pub output: Box<dyn AudioOutput>,
    /// Advertise the hub over mDNS.
    pub advertise: bool,
}

impl fmt::Debug for HubConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HubConfig")
            .field("settings", &self.settings)
            .field("output_format", &self.output.format())
            .field("advertise", &self.advertise)
            .finish()
    }
}

/// Receive statistics of one stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamStats {
    /// Frames missing from the output over the last second (lost and not recovered by
    /// redundancy/FEC, or dropped by a jitter-buffer overflow), in percent.
    pub loss_pct: f32,
    /// Interarrival jitter in ms.
    pub jitter_ms: f32,
    /// Jitter-buffer fill in ms.
    pub buffer_ms: f32,
    /// Estimated end-to-end latency in ms (audio queued in the jitter buffer — its target
    /// before playout starts — + frame + output ring fill target + the output's own
    /// latency).
    pub latency_ms: f32,
    /// Post-gain level in dBFS.
    pub level_db: f32,
}

impl Default for StreamStats {
    fn default() -> Self {
        Self {
            loss_pct: 0.0,
            jitter_ms: 0.0,
            buffer_ms: 0.0,
            latency_ms: 0.0,
            level_db: hfa_audio::meter::SILENCE_DB,
        }
    }
}

/// One incoming stream as shown in the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Stream id.
    pub stream_id: u32,
    /// Sender device id.
    pub device_id: String,
    /// Sender device name.
    pub device_name: String,
    /// Stream label.
    pub label: String,
    /// Sender platform.
    pub platform: String,
    /// Linear gain.
    pub gain: f32,
    /// Muted.
    pub muted: bool,
    /// Priority (ducks the others).
    pub priority: bool,
    /// Packets received within [`IDLE_AFTER`].
    pub active: bool,
    /// Receive statistics.
    pub stats: StreamStats,
}

/// Notifications from a running hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HubEvent {
    /// A stream started.
    SourceAdded(SourceInfo),
    /// A stream stopped or was removed after [`REMOVE_AFTER`].
    SourceRemoved {
        /// The removed stream.
        stream_id: u32,
    },
    /// A stream's controls, activity or statistics changed (stats: about once per second).
    SourceUpdated(SourceInfo),
    /// A sender paired successfully.
    PairingCompleted {
        /// Sender device id.
        device_id: String,
        /// Sender name.
        name: String,
    },
    /// A pairing attempt failed.
    PairingFailed {
        /// Human-readable reason.
        reason: String,
    },
    /// A non-fatal error.
    Error(String),
}

/// Cumulative counters of one stream (diagnostics, `hfa selftest`), see
/// [`HubHandle::stream_counters`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamCounters {
    /// Authenticated datagrams (audio and DTX keep-alives).
    pub datagrams: u64,
    /// DTX keep-alives.
    pub keepalives: u64,
    /// Playout restarts (`FLAG_RESET` or audio resuming after keep-alives).
    pub resets: u64,
    /// Frames decoded from their own packet.
    pub played: u64,
    /// Frames lost in the network (never arrived in time; the jitter buffer's `lost`).
    pub lost: u64,
    /// Packets that arrived after their frame was played.
    pub late: u64,
    /// Duplicate packets.
    pub duplicates: u64,
    /// Lost frames recovered from the redundant copy in the next packet.
    pub recovered_redundancy: u64,
    /// Lost frames recovered with Opus in-band FEC.
    pub recovered_fec: u64,
    /// Frames concealed with PLC (lost and not recovered, or undecodable).
    pub concealed: u64,
    /// Frames synthesized to grow the jitter buffer towards its target.
    pub stretched: u64,
    /// Ticks in which a playing stream ran dry (outside DTX).
    pub underruns: u64,
    /// Buffered frames dropped because the jitter buffer was full (the mixer did not keep
    /// up; not network loss).
    #[serde(default)]
    pub overflowed: u64,
    /// Frames discarded because far more audio was buffered than the target (cuts the
    /// latency a network burst left behind).
    #[serde(default)]
    pub skipped: u64,
}

/// Entry point of the hub engine.
pub struct HubEngine;

impl HubEngine {
    /// Starts the hub on the current tokio runtime: binds TCP and UDP on `settings.port`
    /// (0 = any free port, see [`HubHandle::local_port`]), starts the output and the mixer
    /// thread, and advertises over mDNS if requested (an mDNS failure is only logged).
    ///
    /// # Errors
    /// Invalid settings ([`CoreError::Config`]), a port that is already in use or cannot be
    /// bound ([`CoreError::Io`]), output ([`CoreError::Capture`]) or identity errors.
    pub async fn start(config: HubConfig) -> Result<HubHandle> {
        let HubConfig {
            settings,
            output,
            advertise,
        } = config;
        settings.validate()?;
        let format = output.format();
        if format.sample_rate == 0 || format.channels == 0 {
            return Err(CoreError::Config(format!(
                "output format {} Hz / {} channels is invalid",
                format.sample_rate, format.channels
            )));
        }
        let (identity, trust) = load_identity(&settings).await?;
        let prefs = {
            let (dir, trust) = (settings.data_dir.clone(), trust.clone());
            tokio::task::spawn_blocking(move || crate::hub_prefs::load(&dir, &trust))
                .await
                .unwrap_or_default()
        };
        let (listener, udp) = bind(settings.port).await?;
        let port = listener.local_addr()?.port();

        let (sink, source) = crate::hub_mixer::output_ring(format);
        let underruns = source.stats();
        let mut output = output;
        let (output, started) = tokio::task::spawn_blocking(move || {
            let result = output.start(source);
            (output, result)
        })
        .await
        .map_err(|e| CoreError::Io(format!("output start task failed: {e}")))?;
        started?;

        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let (mixer_tx, mixer_rx) = std::sync::mpsc::channel();
        // The saved master gain (validated above) is the mixer's first command, so the hub
        // plays at the listener's volume from its first frame.
        let _ = mixer_tx.send(MixerCommand::Master(settings.master_gain));
        let mixer_stop = Arc::new(AtomicBool::new(false));
        let output_latency = Arc::new(AtomicU32::new(0));
        let mixer = crate::hub_mixer::spawn(
            output,
            sink,
            underruns,
            mixer_rx,
            events.clone(),
            Arc::clone(&mixer_stop),
            Arc::clone(&output_latency),
        )?;

        let device_id = identity.device_id.clone();
        let pairing =
            PairingManager::new(identity.public_key(), settings.device_name.clone(), port);
        let advertiser = if advertise {
            match Advertiser::start(
                &settings.device_name,
                &device_id,
                port,
                crate::platform_name(),
            ) {
                Ok(a) => Some(a),
                Err(e) => {
                    tracing::warn!(error = %e, "cannot advertise the hub over mDNS");
                    None
                }
            }
        } else {
            None
        };
        tracing::info!(port, %device_id, output = ?format, "hub started");

        let shared = Arc::new(Shared {
            identity,
            trust,
            pairing,
            settings,
            port,
            events: events.clone(),
            state: Mutex::new(HubState {
                sources: Vec::new(),
                prefs,
            }),
            prefs_changed: tokio::sync::Notify::new(),
            demux: Mutex::new(MediaDemux::new()),
            streams: Mutex::new(HashMap::new()),
            mixer: mixer_tx,
            output_latency,
            media_port_override: AtomicU32::new(0),
            epoch: Instant::now(),
            next_conn: AtomicU64::new(1),
            pending: Mutex::new(VecDeque::new()),
        });
        let receiver_stop = Arc::new(AtomicBool::new(false));
        let receiver = spawn_receiver(Arc::clone(&shared), udp, Arc::clone(&receiver_stop))
            .inspect_err(|_| mixer_stop.store(true, Ordering::Release))?;
        let (shutdown, shutdown_rx) = watch::channel(false);
        let tasks = vec![
            tokio::spawn(accept_loop(
                Arc::clone(&shared),
                listener,
                shutdown_rx.clone(),
            )),
            tokio::spawn(stats_loop(Arc::clone(&shared), shutdown_rx.clone())),
            tokio::spawn(prefs_loop(Arc::clone(&shared), shutdown_rx)),
        ];
        Ok(HubHandle {
            events,
            local_port: port,
            device_id,
            shared,
            shutdown,
            tasks,
            receiver: Some(receiver),
            receiver_stop,
            mixer: Some(mixer),
            mixer_stop,
            advertiser,
        })
    }
}

/// Address families the hub listens on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stack {
    /// One IPv6 socket per protocol with `IPV6_V6ONLY` off: IPv6 and (IPv4-mapped) IPv4.
    Dual,
    /// IPv4 only (the host has no IPv6).
    V4,
}

/// A bind failure: which protocol, and the error.
type BindError = (&'static str, std::io::Error);

fn new_socket(stack: Stack, ty: socket2::Type, port: u16) -> std::io::Result<socket2::Socket> {
    use socket2::{Domain, Socket};
    let (domain, addr): (Domain, std::net::SocketAddr) = match stack {
        Stack::Dual => (Domain::IPV6, (Ipv6Addr::UNSPECIFIED, port).into()),
        Stack::V4 => (Domain::IPV4, (Ipv4Addr::UNSPECIFIED, port).into()),
    };
    let socket = Socket::new(domain, ty, None)?;
    if stack == Stack::Dual {
        socket.set_only_v6(false)?;
    }
    // Like `tokio::net::TcpListener::bind`: a restarted hub can take its port back at once
    // (not on Windows, where SO_REUSEADDR would let another socket steal a bound port).
    #[cfg(not(windows))]
    if ty == socket2::Type::STREAM {
        socket.set_reuse_address(true)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    Ok(socket)
}

fn tcp_listener(stack: Stack, port: u16) -> std::io::Result<TcpListener> {
    let socket = new_socket(stack, socket2::Type::STREAM, port)?;
    socket.listen(1024)?;
    TcpListener::from_std(socket.into())
}

/// The media socket, blocking with a [`RECEIVE_POLL`] timeout (the receive thread owns it).
fn udp_socket(stack: Stack, port: u16) -> std::io::Result<std::net::UdpSocket> {
    let socket = new_socket(stack, socket2::Type::DGRAM, port)?;
    crate::media::enlarge_buffers(socket2::SockRef::from(&socket));
    socket.set_nonblocking(false)?;
    socket.set_read_timeout(Some(RECEIVE_POLL))?;
    Ok(socket.into())
}

/// Binds TCP and UDP on the same port number in `stack` (port 0: any number free for both).
fn bind_stack(
    stack: Stack,
    port: u16,
) -> std::result::Result<(TcpListener, std::net::UdpSocket), BindError> {
    if port != 0 {
        let tcp = tcp_listener(stack, port).map_err(|e| ("TCP", e))?;
        let udp = udp_socket(stack, port).map_err(|e| ("UDP", e))?;
        return Ok((tcp, udp));
    }
    for _ in 0..PORT_ATTEMPTS {
        let tcp = tcp_listener(stack, 0).map_err(|e| ("TCP", e))?;
        let chosen = tcp.local_addr().map_err(|e| ("TCP", e))?.port();
        match udp_socket(stack, chosen) {
            Ok(udp) => return Ok((tcp, udp)),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(e) => return Err(("UDP", e)),
        }
    }
    Err((
        "UDP",
        std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "found no port number free for both TCP and UDP",
        ),
    ))
}

/// Binds TCP and UDP on the same port number (see the module docs): dual-stack (IPv6 and
/// IPv4) where the host supports IPv6, else IPv4 only.
async fn bind(port: u16) -> Result<(TcpListener, std::net::UdpSocket)> {
    let port_error = |(proto, e): BindError| {
        if port == 0 {
            CoreError::Io(format!("cannot bind a {proto} port: {e}"))
        } else if e.kind() == std::io::ErrorKind::AddrInUse {
            CoreError::Io(format!(
                "{proto} port {port} is already in use (is another hub running?); choose \
                 another port, or 0 for any free port"
            ))
        } else {
            CoreError::Io(format!("cannot bind {proto} port {port}: {e}"))
        }
    };
    match bind_stack(Stack::Dual, port) {
        Ok(sockets) => Ok(sockets),
        Err((proto, e)) if e.kind() == std::io::ErrorKind::AddrInUse => Err(port_error((proto, e))),
        Err((proto, e)) => {
            tracing::info!(%proto, error = %e, "no IPv6 sockets; the hub listens on IPv4 only");
            bind_stack(Stack::V4, port).map_err(port_error)
        }
    }
}

/// Everything the hub's tasks share.
struct Shared {
    identity: Identity,
    trust: TrustStore,
    pairing: PairingManager,
    settings: Settings,
    port: u16,
    events: broadcast::Sender<HubEvent>,
    state: Mutex<HubState>,
    /// Signalled when the remembered controls changed (see [`prefs_loop`]).
    prefs_changed: tokio::sync::Notify,
    demux: Mutex<MediaDemux>,
    /// Receive-path lookup of the streams' shared state.
    streams: Mutex<HashMap<u32, Arc<StreamShared>>>,
    mixer: std::sync::mpsc::Sender<MixerCommand>,
    /// Latency after the mixer in ms (`f32` bits: output ring fill target + the output's
    /// own latency), written by the mixer thread.
    output_latency: Arc<AtomicU32>,
    /// UDP port announced in `StreamAccepted` instead of the bound one (0 = none).
    media_port_override: AtomicU32,
    epoch: Instant,
    next_conn: AtomicU64,
    /// Connections whose handshake is running, oldest first.
    pending: Mutex<VecDeque<PendingConn>>,
}

/// Lock order: `state` → `demux` → `streams` → a stream's `state`.
struct HubState {
    sources: Vec<SourceEntry>,
    /// Controls remembered per device id (persisted, see [`prefs_loop`]).
    prefs: HashMap<String, Controls>,
}

struct SourceEntry {
    info: SourceInfo,
    /// The sender's static key (its trust is re-checked every stats tick).
    public_key: [u8; 32],
    frame_ms: u32,
    tx: mpsc::UnboundedSender<ConnCommand>,
    shared: Arc<StreamShared>,
    /// Counters at the end of the last loss measurement.
    last: Snapshot,
    /// Network loss of the last measurement (percent).
    loss_pct: f32,
    /// Loss left after recovery of the last measurement (percent).
    residual_loss_pct: f32,
}

#[derive(Debug, Clone, Copy, Default)]
struct Snapshot {
    lost: u64,
    played: u64,
    recovered: u64,
    overflowed: u64,
}

/// Commands to a connection task.
enum ConnCommand {
    Send(ControlMessage),
    Close(String),
}

impl Shared {
    fn emit(&self, event: HubEvent) {
        let _ = self.events.send(event);
    }

    fn udp_port(&self) -> u16 {
        match self.media_port_override.load(Ordering::Relaxed) {
            0 => self.port,
            p => u16::try_from(p).unwrap_or(self.port),
        }
    }

    /// Removes a stream everywhere; emits `SourceRemoved` if it was listed.
    fn remove_stream(&self, stream_id: u32) -> bool {
        let removed = {
            let mut st = self.state.lock();
            let pos = st
                .sources
                .iter()
                .position(|e| e.info.stream_id == stream_id);
            pos.map(|p| st.sources.remove(p))
        };
        self.demux.lock().remove_stream(stream_id);
        self.streams.lock().remove(&stream_id);
        let _ = self.mixer.send(MixerCommand::Remove(stream_id));
        if removed.is_some() {
            tracing::info!(stream_id, "stream removed");
            self.emit(HubEvent::SourceRemoved { stream_id });
            true
        } else {
            false
        }
    }

    /// Validates and registers a `StreamStart`; returns the controls to announce.
    fn start_stream(
        &self,
        peer: &PeerInfo,
        tx: &mpsc::UnboundedSender<ConnCommand>,
        ss: &StreamStart,
    ) -> std::result::Result<Controls, String> {
        if ss.sample_rate != 48_000 || ss.channels != 2 {
            return Err(format!(
                "unsupported format {} Hz / {} channels (48000 Hz stereo required)",
                ss.sample_rate, ss.channels
            ));
        }
        if !FRAME_MS_CHOICES.contains(&ss.frame_ms) {
            return Err(format!("unsupported frame size {} ms", ss.frame_ms));
        }
        let key = MediaKey::from_slice(&ss.media_key).map_err(|_| "invalid media key")?;
        let controls = self
            .state
            .lock()
            .prefs
            .get(&peer.device_id)
            .copied()
            .unwrap_or_default();
        let stream = Arc::new(StreamShared::new(JitterBuffer::new(jitter_config(
            &self.settings,
            ss.frame_ms,
        ))));
        let mix = MixStream::new(ss.stream_id, Arc::clone(&stream), ss.frame_ms, controls)
            .map_err(|e| format!("cannot create the decoder: {e}"))?;
        let label = match hfa_proto::sanitize_name(&ss.label) {
            l if l.trim().is_empty() => "Audio".to_owned(),
            l => l,
        };
        let info = SourceInfo {
            stream_id: ss.stream_id,
            device_id: peer.device_id.clone(),
            device_name: peer.name.clone(),
            label,
            platform: peer.platform.clone(),
            gain: controls.gain,
            muted: controls.muted,
            priority: controls.priority,
            active: true,
            stats: StreamStats::default(),
        };
        self.replace_stale_streams(peer, tx, &info.label);
        {
            let mut st = self.state.lock();
            if st.sources.len() >= MAX_STREAMS {
                return Err(format!("the hub already mixes {MAX_STREAMS} streams"));
            }
            if !self.demux.lock().add_stream(ss.stream_id, &key) {
                return Err("stream id already in use".into());
            }
            self.streams
                .lock()
                .insert(ss.stream_id, Arc::clone(&stream));
            let _ = self.mixer.send(MixerCommand::Add(Box::new(mix)));
            st.sources.push(SourceEntry {
                info: info.clone(),
                public_key: peer.public_key,
                frame_ms: ss.frame_ms,
                tx: tx.clone(),
                shared: stream,
                last: Snapshot::default(),
                loss_pct: 0.0,
                residual_loss_pct: 0.0,
            });
        }
        tracing::info!(
            stream_id = ss.stream_id,
            device = %peer.device_id,
            label = %info.label,
            frame_ms = ss.frame_ms,
            "stream added"
        );
        self.emit(HubEvent::SourceAdded(info));
        Ok(controls)
    }

    /// A device that reconnects (e.g. after roaming to another network) usually leaves its
    /// old connection half-open, so its old stream would linger as an inactive duplicate
    /// until [`REMOVE_AFTER`] and hold a [`MAX_STREAMS`] slot. Removes the streams of the same
    /// device and label on **other** connections that received nothing for [`IDLE_AFTER`],
    /// and closes such a connection once it owns no stream any more.
    fn replace_stale_streams(
        &self,
        peer: &PeerInfo,
        tx: &mpsc::UnboundedSender<ConnCommand>,
        label: &str,
    ) {
        let now = Instant::now();
        let stale: Vec<(u32, mpsc::UnboundedSender<ConnCommand>)> = self
            .state
            .lock()
            .sources
            .iter()
            .filter(|e| {
                e.info.device_id == peer.device_id
                    && e.info.label == label
                    && !e.tx.same_channel(tx)
                    && now.saturating_duration_since(e.shared.state.lock().last_packet)
                        >= IDLE_AFTER
            })
            .map(|e| (e.info.stream_id, e.tx.clone()))
            .collect();
        for (stream_id, old_tx) in stale {
            tracing::info!(stream_id, device = %peer.device_id, "replacing a stale stream");
            self.remove_stream(stream_id);
            let still_used = self
                .state
                .lock()
                .sources
                .iter()
                .any(|e| e.tx.same_channel(&old_tx));
            if !still_used {
                let _ = old_tx.send(ConnCommand::Close("replaced by a new connection".into()));
            }
        }
    }

    /// Changes a stream's controls, remembers them for its device, tells the mixer and the
    /// sender.
    fn update_controls(
        &self,
        stream_id: u32,
        change: impl FnOnce(&mut Controls) -> Body,
    ) -> Result<()> {
        let info = {
            let mut st = self.state.lock();
            let HubState { sources, prefs } = &mut *st;
            let entry = sources
                .iter_mut()
                .find(|e| e.info.stream_id == stream_id)
                .ok_or(CoreError::UnknownStream(stream_id))?;
            let mut c = Controls {
                gain: entry.info.gain,
                muted: entry.info.muted,
                priority: entry.info.priority,
            };
            let body = change(&mut c);
            entry.info.gain = c.gain;
            entry.info.muted = c.muted;
            entry.info.priority = c.priority;
            prefs.insert(entry.info.device_id.clone(), c);
            self.prefs_changed.notify_one();
            // Both sends are non-blocking; doing them under the lock keeps the mixer and the
            // sender in the order in which concurrent calls changed `state` (a slider drag
            // through the FFI runs `set_gain` on several threads).
            let _ = self.mixer.send(MixerCommand::Controls(stream_id, c));
            let _ = entry.tx.send(ConnCommand::Send(ControlMessage::new(body)));
            entry.info.clone()
        };
        self.emit(HubEvent::SourceUpdated(info));
        Ok(())
    }

    /// One stats tick (see the module docs).
    fn collect_stats(&self) {
        let now = Instant::now();
        let output_latency = f32::from_bits(self.output_latency.load(Ordering::Relaxed));
        let mut updates = Vec::new();
        let mut expired = Vec::new();
        {
            let mut st = self.state.lock();
            for e in st.sources.iter_mut() {
                let (jb, buffered, target, primed, mix, level, last_packet) = {
                    let s = e.shared.state.lock();
                    (
                        s.jb.stats(),
                        s.jb.buffered_ms(),
                        s.jb.target_ms(),
                        s.jb.is_primed(),
                        s.mix,
                        s.level_db,
                        s.last_packet,
                    )
                };
                let cur = Snapshot {
                    lost: jb.lost,
                    played: mix.played,
                    recovered: mix.recovered_redundancy + mix.recovered_fec,
                    overflowed: jb.overflowed,
                };
                let lost = cur.lost.saturating_sub(e.last.lost);
                let played = cur.played.saturating_sub(e.last.played);
                let recovered = cur.recovered.saturating_sub(e.last.recovered).min(lost);
                let overflowed = cur.overflowed.saturating_sub(e.last.overflowed);
                let total = lost + played;
                let idle_for = now.saturating_duration_since(last_packet);
                let active = idle_for < IDLE_AFTER;
                // Whether this tick completed a measurement worth sending to the sender. A
                // partial interval is not: re-sending the previous value would count one
                // heavy-loss second twice in the sender's adaptation.
                let mut report = true;
                if !active {
                    // Not even DTX keep-alives (every 100 ms) arrive: everything the sender
                    // sends is lost (UDP blocked by a firewall, or an outage). Say so instead
                    // of "0 %" (see NO_MEDIA_LOSS_PCT).
                    e.last = cur;
                    e.loss_pct = NO_MEDIA_LOSS_PCT;
                    e.residual_loss_pct = 0.0;
                } else if total >= MIN_FRAMES_FOR_LOSS {
                    // Too few frames (start-up, DTX) would give a noisy estimate that the
                    // sender might overreact to: keep the previous values and let the
                    // interval grow until it holds enough frames.
                    e.last = cur;
                    // Network loss only: frames the hub itself dropped (overflow) must not
                    // make the sender cut its bitrate.
                    e.loss_pct = 100.0 * lost as f32 / total as f32;
                    // What the listener misses: unrecovered losses and overflow drops.
                    e.residual_loss_pct = 100.0 * (lost - recovered + overflowed) as f32
                        / (total + overflowed) as f32;
                } else if total == 0 {
                    // Nothing played while keep-alives arrive (DTX): no loss either.
                    e.last = cur;
                    e.loss_pct = 0.0;
                    e.residual_loss_pct = 0.0;
                } else {
                    report = false;
                }
                // The audio actually queued while playing (a burst can leave more than the
                // target for a while), the target before playout starts.
                let queued = if primed { buffered } else { target };
                let latency_ms = queued as f32 + e.frame_ms as f32 + output_latency;
                e.info.active = active;
                e.info.stats = StreamStats {
                    loss_pct: e.residual_loss_pct,
                    jitter_ms: jb.jitter_ms,
                    buffer_ms: buffered as f32,
                    latency_ms,
                    level_db: if active {
                        level
                    } else {
                        hfa_audio::meter::SILENCE_DB
                    },
                };
                if !self.trust.is_trusted_as(&e.public_key, PeerRole::Sender) {
                    // Forgotten on this hub (forget_peer, `hfa trust remove`): trust is only
                    // checked at the handshake, so end what is already connected.
                    expired.push((
                        e.info.stream_id,
                        e.tx.clone(),
                        "this device is no longer trusted by the hub".to_owned(),
                    ));
                    continue;
                }
                if idle_for >= REMOVE_AFTER {
                    expired.push((
                        e.info.stream_id,
                        e.tx.clone(),
                        format!("no media received for {} s", REMOVE_AFTER.as_secs()),
                    ));
                    continue;
                }
                if report {
                    let stats = Stats {
                        stream_id: e.info.stream_id,
                        loss_pct: e.loss_pct,
                        jitter_ms: jb.jitter_ms,
                        buffer_ms: buffered as f32,
                        latency_ms,
                        recommended_bitrate: 0,
                    };
                    let _ =
                        e.tx.send(ConnCommand::Send(ControlMessage::new(Body::Stats(stats))));
                }
                updates.push(e.info.clone());
            }
        }
        for info in updates {
            self.emit(HubEvent::SourceUpdated(info));
        }
        for (stream_id, tx, reason) in expired {
            tracing::info!(stream_id, %reason, "removing the stream");
            self.remove_stream(stream_id);
            let _ = tx.send(ConnCommand::Close(reason));
        }
    }
}

/// Jitter buffer configuration for a stream with `frame_ms` frames.
fn jitter_config(settings: &Settings, frame_ms: u32) -> JitterConfig {
    let min = settings.jitter_min_ms;
    let max = settings.jitter_max_ms.max(min);
    JitterConfig {
        frame_ms,
        min_target_ms: min,
        max_target_ms: max,
        initial_target_ms: INITIAL_JITTER_TARGET_MS.clamp(min, max),
        capacity: ((max / frame_ms.max(1)) as usize + 16).max(64),
    }
}

async fn accept_loop(
    shared: Arc<Shared>,
    listener: TcpListener,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, addr)) => {
                    // An IPv4 peer on the dual-stack socket shows up as ::ffff:a.b.c.d.
                    let ip = addr.ip().to_canonical();
                    // Held across `spawn`, so the task cannot deregister before it is listed.
                    let mut pending = shared.pending.lock();
                    let authenticated = connections.len().saturating_sub(pending.len());
                    if authenticated >= MAX_CONNECTIONS {
                        tracing::debug!(%addr, "too many connections; refusing");
                        continue;
                    }
                    if pending.iter().filter(|p| p.ip == ip).count() >= MAX_PENDING_PER_IP {
                        tracing::debug!(%addr, "too many handshakes from this address; refusing");
                        continue;
                    }
                    if pending.len() >= MAX_PENDING_HANDSHAKES {
                        if let Some(oldest) = pending.pop_front() {
                            tracing::debug!(ip = %oldest.ip, "too many handshakes; dropping the oldest");
                            oldest.abort.abort();
                        }
                    }
                    let _ = stream.set_nodelay(true);
                    let conn = shared.next_conn.fetch_add(1, Ordering::Relaxed);
                    let abort = connections.spawn(connection(
                        Arc::clone(&shared),
                        stream,
                        conn,
                        shutdown.clone(),
                    ));
                    pending.push_back(PendingConn { conn, ip, abort });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "accepting a connection failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            _ = wait_stop(&mut shutdown) => break,
        }
    }
    drop(listener);
    let drained = tokio::time::timeout(CLOSE_TIMEOUT, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
}

/// A connection whose handshake is still running (see [`MAX_PENDING_HANDSHAKES`]).
struct PendingConn {
    conn: u64,
    ip: IpAddr,
    abort: AbortHandle,
}

/// Removes a connection from [`Shared::pending`] when its handshake ends (in any way,
/// including the task being aborted).
struct PendingGuard<'a> {
    shared: &'a Shared,
    conn: u64,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.shared.pending.lock().retain(|p| p.conn != self.conn);
    }
}

/// What the connection task does after a message.
enum Flow {
    Continue,
    /// Close the connection (with `Bye{reason}` if `Some`).
    Close(Option<String>),
}

async fn connection(
    shared: Arc<Shared>,
    stream: TcpStream,
    conn: u64,
    mut shutdown: watch::Receiver<bool>,
) {
    let handshake = PendingGuard {
        shared: &shared,
        conn,
    };
    // A peer that connects and stays silent only holds its slot for a short while.
    let mut first = [0u8; 1];
    let spoke = tokio::select! {
        r = tokio::time::timeout(FIRST_MESSAGE_TIMEOUT, stream.peek(&mut first)) => r,
        _ = wait_stop(&mut shutdown) => return,
    };
    if !matches!(spoke, Ok(Ok(n)) if n > 0) {
        tracing::debug!(conn, "the peer sent nothing; closing");
        return;
    }
    let accepted = tokio::select! {
        r = ControlChannel::accept(stream, &shared.identity, &shared.trust, &shared.pairing) => r,
        _ = wait_stop(&mut shutdown) => return,
    };
    drop(handshake);
    let (mut ch, peer) = match accepted {
        Ok(v) => v,
        Err(CoreError::PairingFailed(reason)) => {
            tracing::info!(%reason, "pairing failed");
            shared.emit(HubEvent::PairingFailed { reason });
            return;
        }
        Err(e) => {
            tracing::debug!(conn, error = %e, "connection refused");
            return;
        }
    };
    tracing::info!(conn, device = %peer.device_id, name = %peer.name, addr = %peer.addr, "sender connected");
    if peer.newly_paired {
        shared.emit(HubEvent::PairingCompleted {
            device_id: peer.device_id.clone(),
            name: peer.name.clone(),
        });
    }
    let (tx, mut rx) = mpsc::unbounded_channel();
    // A sender removed from the trusted devices (e.g. "forget" in the app) is disconnected
    // right away, not only when it reconnects.
    let mut trust_changes = shared.trust.subscribe();
    let mut owned: Vec<u32> = Vec::new();
    // A connection without a stream is closed after NO_STREAM_TIMEOUT (it only holds a slot).
    let mut no_stream_deadline = tokio::time::Instant::now() + NO_STREAM_TIMEOUT;
    let mut idle_deadline = tokio::time::Instant::now() + CONTROL_IDLE_TIMEOUT;
    let close = loop {
        tokio::select! {
            msg = ch.recv() => match msg {
                Ok(msg) => {
                    idle_deadline = tokio::time::Instant::now() + CONTROL_IDLE_TIMEOUT;
                    let flow = on_message(&shared, &peer, &tx, &mut owned, &mut ch, msg).await;
                    if !owned.is_empty() {
                        no_stream_deadline = tokio::time::Instant::now() + NO_STREAM_TIMEOUT;
                    }
                    match flow {
                        Flow::Continue => {}
                        Flow::Close(reason) => break reason,
                    }
                }
                Err(e) => {
                    tracing::debug!(conn, error = %e, "control connection ended");
                    break None;
                }
            },
            cmd = rx.recv() => match cmd {
                Some(ConnCommand::Send(msg)) => {
                    if let Err(e) = ch.send(&msg).await {
                        tracing::debug!(conn, error = %e, "control send failed");
                        break None;
                    }
                }
                Some(ConnCommand::Close(reason)) => break Some(reason),
                None => break None,
            },
            _ = tokio::time::sleep_until(no_stream_deadline), if owned.is_empty() => {
                tracing::debug!(conn, "no stream on this connection; closing it");
                break Some(format!("no stream started for {} s", NO_STREAM_TIMEOUT.as_secs()));
            }
            Ok(()) = trust_changes.changed() => {
                if !shared.trust.is_trusted_as(&peer.public_key, PeerRole::Sender) {
                    tracing::info!(conn, device = %peer.device_id, "sender is no longer trusted; disconnecting it");
                    break Some("device removed from the trusted devices".to_owned());
                }
            }
            _ = tokio::time::sleep_until(idle_deadline) => {
                tracing::info!(conn, device = %peer.device_id, "the sender went silent; closing");
                break Some(format!("nothing received for {} s", CONTROL_IDLE_TIMEOUT.as_secs()));
            }
            _ = wait_stop(&mut shutdown) => break Some("hub stopping".to_owned()),
        }
    };
    for stream_id in owned {
        shared.remove_stream(stream_id);
    }
    if let Some(reason) = close {
        let _ = ch.close(&reason).await;
    }
    tracing::info!(conn, device = %peer.device_id, "sender disconnected");
}

async fn on_message(
    shared: &Shared,
    peer: &PeerInfo,
    tx: &mpsc::UnboundedSender<ConnCommand>,
    owned: &mut Vec<u32>,
    ch: &mut ControlChannel,
    msg: ControlMessage,
) -> Flow {
    let replies = match msg.body {
        Some(Body::StreamStart(ss)) => match shared.start_stream(peer, tx, &ss) {
            Ok(c) => {
                owned.push(ss.stream_id);
                let id = ss.stream_id;
                // The stream's controls are always announced, and before `StreamAccepted`,
                // so the sender takes them over in one step when the stream starts (it may
                // still show the controls of an earlier stream, e.g. from before this hub
                // restarted and forgot them).
                vec![
                    Body::SetVolume(SetVolume {
                        stream_id: id,
                        gain: c.gain,
                    }),
                    Body::SetMute(SetMute {
                        stream_id: id,
                        muted: c.muted,
                    }),
                    Body::SetPriority(SetPriority {
                        stream_id: id,
                        priority: c.priority,
                    }),
                    Body::StreamAccepted(StreamAccepted {
                        stream_id: id,
                        udp_port: u32::from(shared.udp_port()),
                    }),
                ]
            }
            Err(reason) => {
                tracing::info!(stream_id = ss.stream_id, %reason, "stream rejected");
                vec![Body::StreamRejected(StreamRejected {
                    stream_id: ss.stream_id,
                    reason,
                })]
            }
        },
        Some(Body::StreamStop(s)) => {
            if let Some(pos) = owned.iter().position(|id| *id == s.stream_id) {
                owned.swap_remove(pos);
                shared.remove_stream(s.stream_id);
            }
            Vec::new()
        }
        Some(Body::Ping(p)) => vec![Body::Pong(Pong {
            nonce: p.nonce,
            t_us: p.t_us,
        })],
        Some(Body::Bye(b)) => {
            tracing::debug!(device = %peer.device_id, reason = %b.reason, "sender said bye");
            return Flow::Close(None);
        }
        _ => Vec::new(),
    };
    for body in replies {
        if ch.send(&ControlMessage::new(body)).await.is_err() {
            return Flow::Close(None);
        }
    }
    Flow::Continue
}

/// Starts the media receive thread (`hfa-media-rx`) on the hub's UDP socket (blocking, with
/// a [`RECEIVE_POLL`] read timeout). It runs until `stop` is set.
fn spawn_receiver(
    shared: Arc<Shared>,
    socket: std::net::UdpSocket,
    stop: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("hfa-media-rx".into())
        .spawn(move || {
            let _rt = hfa_capture::rt::promote_current_thread(RECEIVE_PERIOD);
            receive_loop(&shared, &socket, &stop);
        })
        .map_err(|e| CoreError::Io(format!("cannot start the media receive thread: {e}")))
}

fn receive_loop(shared: &Shared, socket: &std::net::UdpSocket, stop: &AtomicBool) {
    use std::io::ErrorKind;
    // Room for any UDP datagram: a buffer that is too small makes Windows fail the receive
    // with WSAEMSGSIZE (after consuming the datagram) instead of truncating it.
    let mut buf = vec![0u8; RECV_BUFFER];
    let mut errors_in_a_row = 0u32;
    while !stop.load(Ordering::Acquire) {
        let n = match socket.recv_from(&mut buf) {
            Ok((n, _)) => {
                errors_in_a_row = 0;
                n
            }
            // The read timeout: nothing arrived, look at `stop` again.
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => continue,
            Err(e) => {
                // Per-datagram errors (Windows reports an earlier ICMP "port unreachable" as
                // ConnectionReset, an oversize datagram as WSAEMSGSIZE) must not slow the loop
                // down, or anyone on the LAN could stall all media. Only a long run of errors
                // points to a broken socket and is worth a pause.
                errors_in_a_row = errors_in_a_row.saturating_add(1);
                if errors_in_a_row > ERRORS_BEFORE_BACKOFF {
                    tracing::debug!(error = %e, "UDP receive keeps failing");
                    errors_in_a_row = 0;
                    std::thread::sleep(Duration::from_millis(10));
                }
                continue;
            }
        };
        if n > hfa_proto::MAX_DATAGRAM {
            continue;
        }
        let opened = shared.demux.lock().open(&buf[..n]);
        let Ok((header, payload)) = opened else {
            continue;
        };
        let arrival_us = u64::try_from(shared.epoch.elapsed().as_micros()).unwrap_or(u64::MAX);
        let stream = shared.streams.lock().get(&header.stream_id).cloned();
        if let Some(stream) = stream {
            stream
                .state
                .lock()
                .on_datagram(&header, payload, arrival_us, Instant::now());
        }
    }
}

/// Saves the remembered controls [`PREFS_SAVE_DELAY`] after a change, and once more at
/// shutdown if a change is still pending.
async fn prefs_loop(shared: Arc<Shared>, mut shutdown: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            // A change that is still pending at shutdown is saved (below).
            biased;
            _ = shared.prefs_changed.notified() => {}
            _ = wait_stop(&mut shutdown) => return,
        }
        let stopping = tokio::select! {
            _ = tokio::time::sleep(PREFS_SAVE_DELAY) => false,
            _ = wait_stop(&mut shutdown) => true,
        };
        save_prefs(&shared).await;
        if stopping {
            return;
        }
    }
}

async fn save_prefs(shared: &Arc<Shared>) {
    let prefs = shared.state.lock().prefs.clone();
    let (dir, trust) = (shared.settings.data_dir.clone(), shared.trust.clone());
    let saved =
        tokio::task::spawn_blocking(move || crate::hub_prefs::save(&dir, &prefs, &trust)).await;
    match saved {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "cannot save the remembered hub controls"),
        Err(e) => tracing::warn!(error = %e, "hub controls save task failed"),
    }
}

async fn stats_loop(shared: Arc<Shared>, mut shutdown: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(STATS_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    let mut ticks = 0u32;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                ticks = ticks.wrapping_add(1);
                if ticks.is_multiple_of(TRUST_RELOAD_TICKS) {
                    reload_trust(&shared.trust).await;
                }
                shared.collect_stats();
            }
            _ = wait_stop(&mut shutdown) => break,
        }
    }
}

/// Re-reads the trust store off the async workers, so a peer removed by another process
/// (e.g. `hfa trust remove` next to a running app) is noticed.
pub(crate) async fn reload_trust(trust: &TrustStore) {
    let trust = trust.clone();
    match tokio::task::spawn_blocking(move || trust.reload()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => tracing::debug!(error = %e, "cannot reload the trust store"),
        Err(e) => tracing::debug!(error = %e, "trust reload task failed"),
    }
}

/// Handle to a running hub. `Send + Sync`. Dropping it stops the hub in the background.
pub struct HubHandle {
    events: broadcast::Sender<HubEvent>,
    local_port: u16,
    device_id: String,
    shared: Arc<Shared>,
    shutdown: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
    receiver: Option<std::thread::JoinHandle<()>>,
    receiver_stop: Arc<AtomicBool>,
    mixer: Option<std::thread::JoinHandle<()>>,
    mixer_stop: Arc<AtomicBool>,
    advertiser: Option<Advertiser>,
}

impl fmt::Debug for HubHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HubHandle")
            .field("local_port", &self.local_port)
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

impl HubHandle {
    /// All current streams, in the order they started.
    pub fn sources(&self) -> Vec<SourceInfo> {
        self.shared
            .state
            .lock()
            .sources
            .iter()
            .map(|e| e.info.clone())
            .collect()
    }

    /// Cumulative counters of a stream (diagnostics), `None` for an unknown stream.
    pub fn stream_counters(&self, stream_id: u32) -> Option<StreamCounters> {
        let stream = self.shared.streams.lock().get(&stream_id).cloned()?;
        let s = stream.state.lock();
        let jb = s.jb.stats();
        Some(StreamCounters {
            datagrams: s.net.datagrams,
            keepalives: s.net.keepalives,
            resets: s.net.resets,
            played: s.mix.played,
            lost: jb.lost,
            late: jb.late,
            duplicates: jb.duplicate,
            recovered_redundancy: s.mix.recovered_redundancy,
            recovered_fec: s.mix.recovered_fec,
            concealed: s.mix.concealed,
            stretched: jb.stretched,
            underruns: s.mix.underruns,
            overflowed: jb.overflowed,
            skipped: jb.skipped,
        })
    }

    /// Sets a stream's linear gain (clamped to 0.0..=4.0), remembers it for the device and
    /// informs the sender.
    ///
    /// # Errors
    /// [`CoreError::UnknownStream`]; [`CoreError::Config`] for a non-finite gain.
    pub fn set_gain(&self, stream_id: u32, gain: f32) -> Result<()> {
        if !gain.is_finite() {
            return Err(CoreError::Config(format!("invalid gain {gain}")));
        }
        let gain = gain.clamp(0.0, MAX_GAIN);
        self.shared.update_controls(stream_id, |c| {
            c.gain = gain;
            Body::SetVolume(SetVolume { stream_id, gain })
        })
    }

    /// Mutes/unmutes a stream, remembers it for the device and informs the sender.
    ///
    /// # Errors
    /// [`CoreError::UnknownStream`].
    pub fn set_muted(&self, stream_id: u32, muted: bool) -> Result<()> {
        self.shared.update_controls(stream_id, |c| {
            c.muted = muted;
            Body::SetMute(SetMute { stream_id, muted })
        })
    }

    /// Marks a stream as priority (ducks the others), remembers it for the device and informs
    /// the sender.
    ///
    /// # Errors
    /// [`CoreError::UnknownStream`].
    pub fn set_priority(&self, stream_id: u32, priority: bool) -> Result<()> {
        self.shared.update_controls(stream_id, |c| {
            c.priority = priority;
            Body::SetPriority(SetPriority {
                stream_id,
                priority,
            })
        })
    }

    /// Sets the master linear gain (clamped to 0.0..=4.0; non-finite values are ignored).
    /// Not saved: the hub starts with `Settings::master_gain`, which the caller persists.
    pub fn set_master_gain(&self, gain: f32) {
        if gain.is_finite() {
            let _ = self
                .shared
                .mixer
                .send(MixerCommand::Master(gain.clamp(0.0, MAX_GAIN)));
        }
    }

    /// Announces `port` instead of the bound UDP port in `StreamAccepted` (`None` restores
    /// the bound port). For setups where the media port is forwarded or relayed, e.g. the
    /// network impairment proxy of [`crate::netsim`] in tests and `hfa selftest`.
    pub fn set_media_port_override(&self, port: Option<u16>) {
        self.shared
            .media_port_override
            .store(u32::from(port.unwrap_or(0)), Ordering::Relaxed);
    }

    /// Opens a pairing window ([`crate::pairing::DEFAULT_PAIRING_TTL`]).
    pub fn start_pairing(&self) -> PairingInfo {
        self.shared.pairing.start(DEFAULT_PAIRING_TTL)
    }

    /// The open pairing window, or `None` when none is open (never opened, cancelled,
    /// expired, consumed by a successful pairing, or closed after
    /// [`crate::pairing::MAX_FAILED_ATTEMPTS`] unsuccessful attempts).
    pub fn current_pairing(&self) -> Option<PairingInfo> {
        self.shared.pairing.current()
    }

    /// Closes the pairing window.
    pub fn cancel_pairing(&self) {
        self.shared.pairing.cancel();
    }

    /// Subscribes to events.
    pub fn events(&self) -> broadcast::Receiver<HubEvent> {
        self.events.subscribe()
    }

    /// The bound TCP/UDP port.
    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    /// This hub's device id.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Disconnects all senders (`Bye`), stops advertising, the mixer and the output, and
    /// waits for all tasks.
    pub async fn stop(mut self) {
        let _ = self.shutdown.send(true);
        for task in self.tasks.drain(..) {
            if let Err(e) = task.await {
                tracing::warn!(error = %e, "hub task failed");
            }
        }
        if let Some(advertiser) = self.advertiser.take() {
            if let Err(e) = advertiser.stop() {
                tracing::debug!(error = %e, "stopping mDNS advertising");
            }
        }
        self.receiver_stop.store(true, Ordering::Release);
        if let Some(receiver) = self.receiver.take() {
            if tokio::task::spawn_blocking(move || receiver.join())
                .await
                .is_err()
            {
                tracing::warn!("media receive thread join failed");
            }
        }
        self.mixer_stop.store(true, Ordering::Release);
        if let Some(mixer) = self.mixer.take() {
            if tokio::task::spawn_blocking(move || mixer.join())
                .await
                .is_err()
            {
                tracing::warn!("mixer thread join failed");
            }
        }
        tracing::info!(port = self.local_port, "hub stopped");
    }
}

impl Drop for HubHandle {
    fn drop(&mut self) {
        // After `stop` this is a no-op; otherwise the tasks, the receive thread and the mixer
        // wind down on their own.
        let _ = self.shutdown.send(true);
        self.receiver_stop.store(true, Ordering::Release);
        self.mixer_stop.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_config_follows_the_settings() {
        let mut s = Settings {
            jitter_min_ms: 20,
            jitter_max_ms: 150,
            ..Settings::default()
        };
        let c = jitter_config(&s, 10);
        assert_eq!((c.min_target_ms, c.max_target_ms), (20, 150));
        assert_eq!(c.initial_target_ms, 40);
        assert_eq!(c.capacity, 64);
        s.jitter_min_ms = 60;
        s.jitter_max_ms = 2000;
        let c = jitter_config(&s, 20);
        assert_eq!(c.initial_target_ms, 60);
        assert_eq!(c.capacity, 116);
    }
}
