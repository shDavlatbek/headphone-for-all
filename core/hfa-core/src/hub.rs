//! The hub engine: accepts senders, receives their media, and mixes everything into one
//! output.
//!
//! # Structure
//!
//! - **Sockets:** TCP (control) and UDP (media) are bound on the same port number
//!   (`settings.port`; `0` picks a free number for both, see [`HubHandle::local_port`]). A
//!   port that is already taken is an error ([`crate::CoreError::Io`] naming the port).
//! - **Accept loop** (tokio): one task per sender connection ([`ControlChannel::accept`] with
//!   the hub's [`PairingManager`]; [`HubEvent::PairingCompleted`] /
//!   [`HubEvent::PairingFailed`]). Then:
//!   - `StreamStart` is validated (48 kHz, 2 channels, `frame_ms` 10 or 20, a 32-byte key, at
//!     most [`MAX_STREAMS`] streams, a stream id not in use) and registered with the
//!     [`MediaDemux`], the mixer and the source list, then answered with
//!     `StreamAccepted{udp_port}` (plus `SetVolume`/`SetMute`/`SetPriority` when the device
//!     has remembered controls), else `StreamRejected{reason}`.
//!   - `StreamStop`, `Bye` or a lost connection remove the connection's streams
//!     ([`HubEvent::SourceRemoved`]); `Ping` is answered with `Pong`.
//!   - The hub's own controls ([`HubHandle::set_gain`], ...) are forwarded to the sender as
//!     `SetVolume` / `SetMute` / `SetPriority` so its UI can show them. Controls are
//!     remembered per device id and re-applied when the device reconnects.
//! - **Receive task** (tokio): UDP → [`MediaDemux::open`] (authentication, replay window) →
//!   the stream's jitter buffer (see `hub_mixer.rs`: DTX keep-alives only refresh the
//!   stream's activity; a `FLAG_RESET` packet, or the first audio after keep-alives, restarts
//!   the stream's playout).
//! - **Mixer thread** (`hfa-mixer`, soft real-time, see `hub_mixer.rs`): paced by the output
//!   ring fill; per stream: pop → decode / redundancy / FEC / PLC → drift-corrected resampler
//!   → mixer (gain, mute, priority ducking, limiter) → output format → output ring. It polls
//!   [`AudioOutput::has_error`] and restarts a failed output.
//! - **Stats task:** every [`STATS_INTERVAL`] it refreshes each [`SourceInfo`]
//!   ([`HubEvent::SourceUpdated`]) and sends `Stats` to its sender. `Stats.loss_pct` is the
//!   **network** loss (before recovery, what the sender adapts to); [`StreamStats::loss_pct`]
//!   is the loss left after redundancy/FEC. A stream without datagrams (audio or DTX
//!   keep-alives) for [`IDLE_AFTER`] is inactive; after [`REMOVE_AFTER`] it is removed and
//!   its connection closed.
//! - [`HubHandle::stop`] closes every connection with `Bye`, stops advertising, the tasks,
//!   the mixer and the output.

use std::collections::HashMap;
use std::fmt;
use std::net::Ipv4Addr;
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
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};

use crate::config::{Settings, FRAME_MS_CHOICES};
use crate::control::{ControlChannel, PeerInfo};
use crate::discovery::Advertiser;
use crate::hub_mixer::{Controls, MixStream, MixerCommand, StreamShared, MIX_FRAME_MS};
use crate::identity::{Identity, TrustStore};
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
/// Most streams the hub mixes at once.
pub const MAX_STREAMS: usize = 16;
/// Most simultaneous control connections (including ones still in the handshake).
pub const MAX_CONNECTIONS: usize = 64;
/// Jitter-buffer target before the first jitter estimate (clamped to the settings' bounds).
const INITIAL_JITTER_TARGET_MS: u32 = 40;
/// Capacity of the event channel.
const EVENT_CAPACITY: usize = 256;
/// Fewest frames a loss measurement covers (a shorter interval is extended).
const MIN_FRAMES_FOR_LOSS: u64 = 40;
/// How often a free TCP port is tried when `settings.port` is 0 and its UDP twin is taken.
const PORT_ATTEMPTS: usize = 16;
/// How long [`HubHandle::stop`] waits for the connections to say `Bye`.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// Packet loss (after FEC) over the last second, in percent.
    pub loss_pct: f32,
    /// Interarrival jitter in ms.
    pub jitter_ms: f32,
    /// Jitter-buffer fill in ms.
    pub buffer_ms: f32,
    /// Estimated end-to-end latency in ms (buffer + frame + output latency).
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
    /// Frames the jitter buffer reported lost (never arrived, too late, dropped by overflow).
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
        let (listener, udp) = bind(settings.port).await?;
        let port = listener.local_addr()?.port();

        let (sink, source) = crate::hub_mixer::output_ring(format);
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
        let mixer_stop = Arc::new(AtomicBool::new(false));
        let output_latency = Arc::new(AtomicU32::new(0));
        let mixer = crate::hub_mixer::spawn(
            output,
            sink,
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
            state: Mutex::new(HubState::default()),
            demux: Mutex::new(MediaDemux::new()),
            streams: Mutex::new(HashMap::new()),
            mixer: mixer_tx,
            output_latency,
            media_port_override: AtomicU32::new(0),
            epoch: Instant::now(),
            next_conn: AtomicU64::new(1),
        });
        let (shutdown, shutdown_rx) = watch::channel(false);
        let tasks = vec![
            tokio::spawn(accept_loop(
                Arc::clone(&shared),
                listener,
                shutdown_rx.clone(),
            )),
            tokio::spawn(receive_loop(
                Arc::clone(&shared),
                Arc::new(udp),
                shutdown_rx.clone(),
            )),
            tokio::spawn(stats_loop(Arc::clone(&shared), shutdown_rx)),
        ];
        Ok(HubHandle {
            events,
            local_port: port,
            device_id,
            shared,
            shutdown,
            tasks,
            mixer: Some(mixer),
            mixer_stop,
            advertiser,
        })
    }
}

/// Binds TCP and UDP on the same port number (see the module docs).
async fn bind(port: u16) -> Result<(TcpListener, UdpSocket)> {
    let any = Ipv4Addr::UNSPECIFIED;
    let port_error = |proto: &str, e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            CoreError::Io(format!(
                "{proto} port {port} is already in use (is another hub running?); choose \
                 another port, or 0 for any free port"
            ))
        } else {
            CoreError::Io(format!("cannot bind {proto} port {port}: {e}"))
        }
    };
    if port != 0 {
        let tcp = TcpListener::bind((any, port))
            .await
            .map_err(|e| port_error("TCP", e))?;
        let udp = UdpSocket::bind((any, port))
            .await
            .map_err(|e| port_error("UDP", e))?;
        return Ok((tcp, udp));
    }
    for _ in 0..PORT_ATTEMPTS {
        let tcp = TcpListener::bind((any, 0)).await?;
        let chosen = tcp.local_addr()?.port();
        match UdpSocket::bind((any, chosen)).await {
            Ok(udp) => return Ok((tcp, udp)),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(CoreError::Io(
        "found no port number free for both TCP and UDP".into(),
    ))
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
    demux: Mutex<MediaDemux>,
    /// Receive-path lookup of the streams' shared state.
    streams: Mutex<HashMap<u32, Arc<StreamShared>>>,
    mixer: std::sync::mpsc::Sender<MixerCommand>,
    /// Output latency in ms (`f32` bits), written by the mixer thread.
    output_latency: Arc<AtomicU32>,
    /// UDP port announced in `StreamAccepted` instead of the bound one (0 = none).
    media_port_override: AtomicU32,
    epoch: Instant,
    next_conn: AtomicU64,
}

/// Lock order: `state` → `demux` → `streams` → a stream's `state`.
#[derive(Default)]
struct HubState {
    sources: Vec<SourceEntry>,
    /// Controls remembered per device id.
    prefs: HashMap<String, Controls>,
}

struct SourceEntry {
    info: SourceInfo,
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

    /// Changes a stream's controls, remembers them for its device, tells the mixer and the
    /// sender.
    fn update_controls(
        &self,
        stream_id: u32,
        change: impl FnOnce(&mut Controls) -> Body,
    ) -> Result<()> {
        let (info, tx, controls, body) = {
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
            (entry.info.clone(), entry.tx.clone(), c, body)
        };
        let _ = self.mixer.send(MixerCommand::Controls(stream_id, controls));
        let _ = tx.send(ConnCommand::Send(ControlMessage::new(body)));
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
                let (jb, buffered, target, mix, level, last_packet) = {
                    let s = e.shared.state.lock();
                    (
                        s.jb.stats(),
                        s.jb.buffered_ms(),
                        s.jb.target_ms(),
                        s.mix,
                        s.level_db,
                        s.last_packet,
                    )
                };
                let cur = Snapshot {
                    lost: jb.lost,
                    played: mix.played,
                    recovered: mix.recovered_redundancy + mix.recovered_fec,
                };
                let lost = cur.lost.saturating_sub(e.last.lost);
                let played = cur.played.saturating_sub(e.last.played);
                let recovered = cur.recovered.saturating_sub(e.last.recovered).min(lost);
                let total = lost + played;
                if total >= MIN_FRAMES_FOR_LOSS {
                    // Too few frames (start-up, DTX) would give a noisy estimate that the
                    // sender might overreact to: keep the previous values and let the
                    // interval grow until it holds enough frames.
                    e.last = cur;
                    e.loss_pct = 100.0 * lost as f32 / total as f32;
                    e.residual_loss_pct = 100.0 * (lost - recovered) as f32 / total as f32;
                } else if total == 0 {
                    // Nothing played (DTX or idle): no loss either.
                    e.last = cur;
                    e.loss_pct = 0.0;
                    e.residual_loss_pct = 0.0;
                }
                let idle_for = now.saturating_duration_since(last_packet);
                let active = idle_for < IDLE_AFTER;
                let latency_ms =
                    target as f32 + e.frame_ms as f32 + 2.0 * MIX_FRAME_MS as f32 + output_latency;
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
                if idle_for >= REMOVE_AFTER {
                    expired.push((e.info.stream_id, e.tx.clone()));
                    continue;
                }
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
                updates.push(e.info.clone());
            }
        }
        for info in updates {
            self.emit(HubEvent::SourceUpdated(info));
        }
        for (stream_id, tx) in expired {
            tracing::info!(
                stream_id,
                "no media for {} s; removing the stream",
                REMOVE_AFTER.as_secs()
            );
            self.remove_stream(stream_id);
            let _ = tx.send(ConnCommand::Close(format!(
                "no media received for {} s",
                REMOVE_AFTER.as_secs()
            )));
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
                    if connections.len() >= MAX_CONNECTIONS {
                        tracing::debug!(%addr, "too many connections; refusing");
                        continue;
                    }
                    let _ = stream.set_nodelay(true);
                    let id = shared.next_conn.fetch_add(1, Ordering::Relaxed);
                    connections.spawn(connection(Arc::clone(&shared), stream, id, shutdown.clone()));
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
    let accepted = tokio::select! {
        r = ControlChannel::accept(stream, &shared.identity, &shared.trust, &shared.pairing) => r,
        _ = wait_stop(&mut shutdown) => return,
    };
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
    let mut owned: Vec<u32> = Vec::new();
    let close = loop {
        tokio::select! {
            msg = ch.recv() => match msg {
                Ok(msg) => match on_message(&shared, &peer, &tx, &mut owned, &mut ch, msg).await {
                    Flow::Continue => {}
                    Flow::Close(reason) => break reason,
                },
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
                let mut replies = vec![Body::StreamAccepted(StreamAccepted {
                    stream_id: id,
                    udp_port: u32::from(shared.udp_port()),
                })];
                let d = Controls::default();
                if c.gain != d.gain {
                    replies.push(Body::SetVolume(SetVolume {
                        stream_id: id,
                        gain: c.gain,
                    }));
                }
                if c.muted {
                    replies.push(Body::SetMute(SetMute {
                        stream_id: id,
                        muted: true,
                    }));
                }
                if c.priority {
                    replies.push(Body::SetPriority(SetPriority {
                        stream_id: id,
                        priority: true,
                    }));
                }
                replies
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

async fn receive_loop(
    shared: Arc<Shared>,
    socket: Arc<UdpSocket>,
    mut shutdown: watch::Receiver<bool>,
) {
    // One byte more than any valid datagram, so oversize ones are seen (and rejected).
    let mut buf = vec![0u8; hfa_proto::MAX_DATAGRAM + 1];
    loop {
        let received = tokio::select! {
            r = socket.recv_from(&mut buf) => r,
            _ = wait_stop(&mut shutdown) => break,
        };
        let n = match received {
            Ok((n, _)) => n,
            Err(e) => {
                // Windows reports an earlier ICMP "port unreachable" as ConnectionReset.
                if e.kind() != std::io::ErrorKind::ConnectionReset {
                    tracing::debug!(error = %e, "UDP receive failed");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                continue;
            }
        };
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

async fn stats_loop(shared: Arc<Shared>, mut shutdown: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(STATS_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    loop {
        tokio::select! {
            _ = tick.tick() => shared.collect_stats(),
            _ = wait_stop(&mut shutdown) => break,
        }
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
        // After `stop` this is a no-op; otherwise the tasks and the mixer wind down on their own.
        let _ = self.shutdown.send(true);
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
