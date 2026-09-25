//! Conversions between engine types and the API DTOs (kept out of `crate::api` so
//! flutter_rust_bridge does not export them).

use hfa_audio::AudioFormat;
use hfa_capture::{Capabilities, CaptureTarget, OutputTarget};
use hfa_core::discovery::{DiscoveryEvent, HubInfo};
use hfa_core::{
    HubEvent, PairingInfo, SenderState, SenderStatus, Settings, SourceInfo, TrustedPeer,
};

use crate::api::app::{CapabilitiesDto, PairingUriDto, SettingsDto, TrustedPeerDto};
use crate::api::hub::{HubEventDto, HubStatusDto, PairingInfoDto, SourceDto};
use crate::api::sender::{CaptureSourceDto, DiscoveryEventDto, HubInfoDto, SenderStatusDto};
use crate::error::{FfiError, Result};
use crate::hub_target::encode_key;
use crate::pcm::format_is_valid;
use crate::sender_meta::SenderMeta;

/// Accepted bitrates (bit/s), as in `hfa send --bitrate`.
pub const BITRATE_RANGE: std::ops::RangeInclusive<u32> = 6_000..=510_000;
/// Accepted Opus frame lengths (ms).
pub const FRAME_MS_CHOICES: [u8; 2] = [10, 20];
/// Upper bound of the jitter-buffer bounds (ms).
pub const MAX_JITTER_MS: u32 = 2_000;
/// Longest device name, in characters.
pub const MAX_DEVICE_NAME_CHARS: usize = 64;

/// Unix seconds as the `i64` the Dart side uses (`int`), saturating.
fn unix_i64(secs: u64) -> i64 {
    i64::try_from(secs).unwrap_or(i64::MAX)
}

/// [`Capabilities`] → [`CapabilitiesDto`]. `external_only` is set on Android and iOS, where
/// capture exists only as native code feeding an external feed.
pub(crate) fn capabilities_dto(c: Capabilities) -> CapabilitiesDto {
    CapabilitiesDto {
        system_mix: c.system_mix,
        per_app: c.per_app,
        mutes_local_output: c.mutes_local_output,
        external_only: cfg!(any(target_os = "android", target_os = "ios")),
        notes: c.notes,
    }
}

/// [`Settings`] → [`SettingsDto`]. Only device outputs have a DTO form; `default`, WAV and
/// null outputs (set by the CLI) show as `None`.
pub(crate) fn settings_dto(s: &Settings) -> SettingsDto {
    SettingsDto {
        device_name: s.device_name.clone(),
        port: s.port,
        bitrate: s.bitrate,
        frame_ms: u8::try_from(s.frame_ms).unwrap_or(u8::MAX),
        fec: s.fec,
        jitter_min_ms: s.jitter_min_ms,
        jitter_max_ms: s.jitter_max_ms,
        output_device: match &s.output {
            OutputTarget::Device(name) => Some(name.clone()),
            _ => None,
        },
    }
}

/// Validates `dto` and returns `current` with its values applied (`data_dir` is kept).
/// `output_device: None` selects the OS default output.
///
/// # Errors
/// [`FfiError::InvalidArgument`] naming the first invalid field.
pub(crate) fn apply_settings(current: &Settings, dto: &SettingsDto) -> Result<Settings> {
    let bad = |msg: String| Err(FfiError::InvalidArgument(msg));
    let name = dto.device_name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_DEVICE_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return bad(format!(
            "device_name must be 1..={MAX_DEVICE_NAME_CHARS} characters without control characters"
        ));
    }
    if !BITRATE_RANGE.contains(&dto.bitrate) {
        return bad(format!(
            "bitrate {} outside {}..={}",
            dto.bitrate,
            BITRATE_RANGE.start(),
            BITRATE_RANGE.end()
        ));
    }
    if !FRAME_MS_CHOICES.contains(&dto.frame_ms) {
        return bad(format!("frame_ms {} must be 10 or 20", dto.frame_ms));
    }
    if dto.jitter_min_ms == 0
        || dto.jitter_min_ms > dto.jitter_max_ms
        || dto.jitter_max_ms > MAX_JITTER_MS
    {
        return bad(format!(
            "jitter bounds {}..={} ms must satisfy 1 <= min <= max <= {MAX_JITTER_MS}",
            dto.jitter_min_ms, dto.jitter_max_ms
        ));
    }
    let output = match dto.output_device.as_deref().map(str::trim) {
        None | Some("") => OutputTarget::Default,
        Some(device) => OutputTarget::Device(device.to_owned()),
    };
    Ok(Settings {
        device_name: name.to_owned(),
        port: dto.port,
        bitrate: dto.bitrate,
        frame_ms: u32::from(dto.frame_ms),
        fec: dto.fec,
        jitter_min_ms: dto.jitter_min_ms,
        jitter_max_ms: dto.jitter_max_ms,
        output,
        data_dir: current.data_dir.clone(),
    })
}

/// [`TrustedPeer`] → [`TrustedPeerDto`].
pub(crate) fn trusted_peer_dto(p: &TrustedPeer) -> TrustedPeerDto {
    TrustedPeerDto {
        device_id: p.device_id.clone(),
        name: p.name.clone(),
        paired_at_unix: unix_i64(p.paired_at),
    }
}

/// [`hfa_proto::PairingUri`] → [`PairingUriDto`].
pub(crate) fn pairing_uri_dto(u: &hfa_proto::PairingUri) -> PairingUriDto {
    PairingUriDto {
        host: u.host.clone(),
        port: u.port,
        hub_id: encode_key(&u.hub_id),
        hub_device_id: hfa_proto::fingerprint(&u.hub_id),
        token: u.token.clone(),
        name: u.name.clone(),
    }
}

/// [`SourceInfo`] → [`SourceDto`] (statistics flattened).
pub(crate) fn source_dto(s: &SourceInfo) -> SourceDto {
    SourceDto {
        stream_id: s.stream_id,
        device_id: s.device_id.clone(),
        device_name: s.device_name.clone(),
        label: s.label.clone(),
        platform: s.platform.clone(),
        gain: s.gain,
        muted: s.muted,
        priority: s.priority,
        active: s.active,
        loss_pct: s.stats.loss_pct,
        jitter_ms: s.stats.jitter_ms,
        buffer_ms: s.stats.buffer_ms,
        latency_ms: s.stats.latency_ms,
        level_db: s.stats.level_db,
    }
}

/// [`PairingInfo`] → [`PairingInfoDto`].
pub(crate) fn pairing_info_dto(p: &PairingInfo) -> PairingInfoDto {
    PairingInfoDto {
        pin: p.pin.clone(),
        token: p.token.clone(),
        uri: p.uri.clone(),
        expires_at_unix: unix_i64(p.expires_at_unix),
    }
}

/// [`HubEvent`] → [`HubEventDto`].
pub(crate) fn hub_event_dto(e: &HubEvent) -> HubEventDto {
    match e {
        HubEvent::SourceAdded(s) => HubEventDto::SourceAdded(source_dto(s)),
        HubEvent::SourceRemoved { stream_id } => HubEventDto::SourceRemoved {
            stream_id: *stream_id,
        },
        HubEvent::SourceUpdated(s) => HubEventDto::SourceUpdated(source_dto(s)),
        HubEvent::PairingCompleted { device_id, name } => HubEventDto::PairingCompleted {
            device_id: device_id.clone(),
            name: name.clone(),
        },
        HubEvent::PairingFailed { reason } => HubEventDto::PairingFailed {
            reason: reason.clone(),
        },
        HubEvent::Error(message) => HubEventDto::Error {
            message: message.clone(),
        },
    }
}

/// [`HubInfo`] → [`HubInfoDto`]; `trusted` says whether the hub is paired.
pub(crate) fn hub_info_dto(h: &HubInfo, trusted: bool) -> HubInfoDto {
    HubInfoDto {
        device_id: h.device_id.clone(),
        name: h.name.clone(),
        addrs: h.addrs.iter().map(ToString::to_string).collect(),
        port: h.port,
        platform: h.platform.clone(),
        trusted,
    }
}

/// [`DiscoveryEvent`] → [`DiscoveryEventDto`]; `is_trusted` tells paired device ids.
pub(crate) fn discovery_event_dto(
    e: &DiscoveryEvent,
    is_trusted: impl Fn(&str) -> bool,
) -> DiscoveryEventDto {
    match e {
        DiscoveryEvent::Found(h) => {
            DiscoveryEventDto::Found(hub_info_dto(h, is_trusted(&h.device_id)))
        }
        DiscoveryEvent::Lost(device_id) => DiscoveryEventDto::Lost {
            device_id: device_id.clone(),
        },
    }
}

/// The state string of [`SenderStatusDto::state`] and the failure reason, if any.
pub(crate) fn sender_state_str(s: &SenderState) -> (&'static str, Option<&str>) {
    crate::sender_meta::state_str(s)
}

/// Builds a [`SenderStatusDto`] from a status and the facts folded from the sender's
/// events. The error is the failure reason when failed, else the last non-fatal error.
pub(crate) fn sender_status_dto(s: &SenderStatus, meta: &SenderMeta) -> SenderStatusDto {
    let (state, _) = sender_state_str(&s.state);
    SenderStatusDto {
        state: state.to_owned(),
        error: meta.error_for(&s.state).map(str::to_owned),
        hub_name: meta.hub_name.clone(),
        bitrate: s.bitrate,
        loss_pct: s.loss_pct,
        rtt_ms: s.rtt_ms,
        level_db: s.level_db,
        hub_gain: meta.hub.gain,
        hub_muted: meta.hub.muted,
        hub_priority: meta.hub.priority,
    }
}

/// The hub status while no hub runs.
pub(crate) fn stopped_hub_status(device_name: String) -> HubStatusDto {
    HubStatusDto {
        running: false,
        port: 0,
        device_name,
        source_count: 0,
        advertised: false,
        advertise_error: None,
    }
}

/// The status reported while no sender exists.
pub(crate) fn idle_sender_status() -> SenderStatusDto {
    SenderStatusDto {
        state: "idle".to_owned(),
        error: None,
        hub_name: None,
        bitrate: 0,
        loss_pct: 0.0,
        rtt_ms: 0.0,
        level_db: hfa_audio::meter::SILENCE_DB,
        hub_gain: 1.0,
        hub_muted: false,
        hub_priority: false,
    }
}

/// [`CaptureSourceDto`] → [`CaptureTarget`], plus the external feed format to register.
///
/// # Errors
/// [`FfiError::InvalidArgument`] for a tone frequency outside (0, 24000) Hz or an external
/// format outside 1..=8 channels / 8000..=192000 Hz.
pub(crate) fn capture_target(
    src: &CaptureSourceDto,
) -> Result<(CaptureTarget, Option<AudioFormat>)> {
    Ok(match src {
        CaptureSourceDto::System => (CaptureTarget::SystemMix, None),
        CaptureSourceDto::SystemExcludingSelf => (CaptureTarget::SystemMixExcludingSelf, None),
        CaptureSourceDto::Process { pid } => (CaptureTarget::Process { pid: *pid }, None),
        CaptureSourceDto::Tone { freq_hz } => {
            if !(freq_hz.is_finite() && *freq_hz > 0.0 && *freq_hz < 24_000.0) {
                return Err(FfiError::InvalidArgument(format!(
                    "tone frequency {freq_hz} Hz outside (0, 24000)"
                )));
            }
            (CaptureTarget::Tone { freq_hz: *freq_hz }, None)
        }
        CaptureSourceDto::External {
            feed_id,
            sample_rate,
            channels,
        } => {
            if !format_is_valid(u32::from(*channels), *sample_rate) {
                return Err(FfiError::InvalidArgument(format!(
                    "external feed format {channels} ch / {sample_rate} Hz"
                )));
            }
            (
                CaptureTarget::External { id: *feed_id },
                Some(AudioFormat::new(*sample_rate, *channels)),
            )
        }
    })
}

/// The label a stream gets when the caller leaves it empty.
pub(crate) fn default_label(src: &CaptureSourceDto) -> String {
    match src {
        CaptureSourceDto::System | CaptureSourceDto::SystemExcludingSelf => {
            "System audio".to_owned()
        }
        CaptureSourceDto::Process { pid } => format!("App {pid}"),
        CaptureSourceDto::Tone { freq_hz } => format!("Tone {freq_hz} Hz"),
        CaptureSourceDto::External { .. } => "Device audio".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hfa_core::StreamStats;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::path::PathBuf;

    fn settings() -> Settings {
        Settings {
            device_name: "Desk".into(),
            port: 47810,
            bitrate: 128_000,
            frame_ms: 10,
            fec: true,
            jitter_min_ms: 20,
            jitter_max_ms: 150,
            output: OutputTarget::Device("USB Headset".into()),
            data_dir: PathBuf::from("/data/hfa"),
        }
    }

    fn source() -> SourceInfo {
        SourceInfo {
            stream_id: 9,
            device_id: "ab12-cd34-ef56-7890".into(),
            device_name: "Laptop".into(),
            label: "System audio".into(),
            platform: "linux".into(),
            gain: 0.5,
            muted: true,
            priority: false,
            active: true,
            stats: StreamStats {
                loss_pct: 1.5,
                jitter_ms: 3.0,
                buffer_ms: 40.0,
                latency_ms: 62.0,
                level_db: -18.0,
            },
        }
    }

    #[test]
    fn settings_round_trip() {
        let s = settings();
        let dto = settings_dto(&s);
        assert_eq!(dto.output_device.as_deref(), Some("USB Headset"));
        assert_eq!(dto.frame_ms, 10);
        let back = apply_settings(&s, &dto).expect("valid");
        assert_eq!(back, s);
    }

    #[test]
    fn settings_default_output_and_trimming() {
        let s = settings();
        let dto = SettingsDto {
            device_name: "  Kitchen  ".into(),
            output_device: Some("  ".into()),
            frame_ms: 20,
            ..settings_dto(&s)
        };
        let out = apply_settings(&s, &dto).expect("valid");
        assert_eq!(out.device_name, "Kitchen");
        assert_eq!(out.output, OutputTarget::Default);
        assert_eq!(out.frame_ms, 20);
        assert_eq!(out.data_dir, s.data_dir);
        let null = Settings {
            output: OutputTarget::Null,
            ..settings()
        };
        assert_eq!(settings_dto(&null).output_device, None);
    }

    #[test]
    fn settings_validation() {
        let s = settings();
        let ok = settings_dto(&s);
        let cases = [
            SettingsDto {
                device_name: " ".into(),
                ..ok.clone()
            },
            SettingsDto {
                device_name: "x".repeat(65),
                ..ok.clone()
            },
            SettingsDto {
                device_name: "a\nb".into(),
                ..ok.clone()
            },
            SettingsDto {
                bitrate: 5_999,
                ..ok.clone()
            },
            SettingsDto {
                bitrate: 510_001,
                ..ok.clone()
            },
            SettingsDto {
                frame_ms: 15,
                ..ok.clone()
            },
            SettingsDto {
                jitter_min_ms: 0,
                ..ok.clone()
            },
            SettingsDto {
                jitter_min_ms: 200,
                jitter_max_ms: 100,
                ..ok.clone()
            },
            SettingsDto {
                jitter_max_ms: 2_001,
                ..ok.clone()
            },
        ];
        for dto in cases {
            assert!(
                matches!(apply_settings(&s, &dto), Err(FfiError::InvalidArgument(_))),
                "{dto:?} must be rejected"
            );
        }
        let edge = SettingsDto {
            device_name: "é".repeat(64),
            bitrate: 6_000,
            jitter_min_ms: 2_000,
            jitter_max_ms: 2_000,
            port: 0,
            ..ok
        };
        assert!(apply_settings(&s, &edge).is_ok());
    }

    #[test]
    fn source_and_events() {
        let dto = source_dto(&source());
        assert_eq!(dto.stream_id, 9);
        assert_eq!(dto.device_name, "Laptop");
        assert!(dto.muted && dto.active && !dto.priority);
        assert_eq!(
            (
                dto.loss_pct,
                dto.jitter_ms,
                dto.buffer_ms,
                dto.latency_ms,
                dto.level_db
            ),
            (1.5, 3.0, 40.0, 62.0, -18.0)
        );
        assert_eq!(
            hub_event_dto(&HubEvent::SourceAdded(source())),
            HubEventDto::SourceAdded(dto.clone())
        );
        assert_eq!(
            hub_event_dto(&HubEvent::SourceUpdated(source())),
            HubEventDto::SourceUpdated(dto)
        );
        assert_eq!(
            hub_event_dto(&HubEvent::SourceRemoved { stream_id: 4 }),
            HubEventDto::SourceRemoved { stream_id: 4 }
        );
        assert_eq!(
            hub_event_dto(&HubEvent::PairingCompleted {
                device_id: "id".into(),
                name: "Phone".into()
            }),
            HubEventDto::PairingCompleted {
                device_id: "id".into(),
                name: "Phone".into()
            }
        );
        assert_eq!(
            hub_event_dto(&HubEvent::PairingFailed {
                reason: "wrong PIN".into()
            }),
            HubEventDto::PairingFailed {
                reason: "wrong PIN".into()
            }
        );
        assert_eq!(
            hub_event_dto(&HubEvent::Error("output lost".into())),
            HubEventDto::Error {
                message: "output lost".into()
            }
        );
    }

    #[test]
    fn sender_status_strings() {
        let cases = [
            (SenderState::Connecting, "connecting"),
            (SenderState::Pairing, "pairing"),
            (SenderState::Streaming, "streaming"),
            (SenderState::Reconnecting, "reconnecting"),
            (SenderState::Stopped, "stopped"),
        ];
        for (state, text) in cases {
            let s = SenderStatus {
                state,
                bitrate: 96_000,
                loss_pct: 2.0,
                rtt_ms: 4.0,
                level_db: -20.0,
            };
            let mut meta = SenderMeta::new(s.clone(), Some("transient".into()));
            meta.hub_name = Some("Hub".into());
            let dto = sender_status_dto(&s, &meta);
            assert_eq!(dto.state, text);
            assert_eq!(dto.error.as_deref(), Some("transient"));
            assert_eq!(dto.hub_name.as_deref(), Some("Hub"));
            assert_eq!(
                (dto.bitrate, dto.loss_pct, dto.rtt_ms, dto.level_db),
                (96_000, 2.0, 4.0, -20.0)
            );
        }
        let failed = SenderStatus {
            state: SenderState::Failed("pairing failed".into()),
            ..SenderStatus::default()
        };
        let mut meta = SenderMeta::new(failed.clone(), Some("older error".into()));
        meta.apply(hfa_core::SenderEvent::HubControl {
            gain: 0.5,
            muted: true,
            priority: true,
        });
        let dto = sender_status_dto(&failed, &meta);
        assert_eq!(dto.state, "failed");
        assert_eq!(dto.error.as_deref(), Some("pairing failed"));
        assert_eq!(
            (dto.hub_gain, dto.hub_muted, dto.hub_priority),
            (0.5, true, true)
        );
        let idle = idle_sender_status();
        assert_eq!(idle.state, "idle");
        assert_eq!(
            (idle.hub_gain, idle.hub_muted, idle.hub_priority),
            (1.0, false, false)
        );
        assert_eq!(idle.level_db, hfa_audio::meter::SILENCE_DB);
    }

    #[test]
    fn capture_sources() {
        assert_eq!(
            capture_target(&CaptureSourceDto::System).expect("ok"),
            (CaptureTarget::SystemMix, None)
        );
        assert_eq!(
            capture_target(&CaptureSourceDto::SystemExcludingSelf).expect("ok"),
            (CaptureTarget::SystemMixExcludingSelf, None)
        );
        assert_eq!(
            capture_target(&CaptureSourceDto::Process { pid: 77 }).expect("ok"),
            (CaptureTarget::Process { pid: 77 }, None)
        );
        assert_eq!(
            capture_target(&CaptureSourceDto::Tone { freq_hz: 440.0 }).expect("ok"),
            (CaptureTarget::Tone { freq_hz: 440.0 }, None)
        );
        assert_eq!(
            capture_target(&CaptureSourceDto::External {
                feed_id: 3,
                sample_rate: 44_100,
                channels: 2
            })
            .expect("ok"),
            (
                CaptureTarget::External { id: 3 },
                Some(AudioFormat::new(44_100, 2))
            )
        );
        for bad in [
            CaptureSourceDto::Tone { freq_hz: 0.0 },
            CaptureSourceDto::Tone { freq_hz: f32::NAN },
            CaptureSourceDto::Tone { freq_hz: 24_000.0 },
            CaptureSourceDto::External {
                feed_id: 1,
                sample_rate: 48_000,
                channels: 0,
            },
            CaptureSourceDto::External {
                feed_id: 1,
                sample_rate: 4_000,
                channels: 2,
            },
        ] {
            assert!(capture_target(&bad).is_err(), "{bad:?}");
        }
        assert_eq!(default_label(&CaptureSourceDto::System), "System audio");
        assert_eq!(
            default_label(&CaptureSourceDto::Process { pid: 5 }),
            "App 5"
        );
        assert_eq!(
            default_label(&CaptureSourceDto::Tone { freq_hz: 440.0 }),
            "Tone 440 Hz"
        );
    }

    #[test]
    fn discovery_and_pairing() {
        let info = HubInfo {
            device_id: "ab12-cd34-ef56-7890".into(),
            name: "Desk".into(),
            addrs: vec![
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5)),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ],
            port: 47810,
            platform: "windows".into(),
        };
        let found = discovery_event_dto(&DiscoveryEvent::Found(info), |id| {
            id == "ab12-cd34-ef56-7890"
        });
        let DiscoveryEventDto::Found(dto) = found else {
            panic!("expected Found");
        };
        assert_eq!(dto.addrs, vec!["192.168.1.5".to_owned(), "::1".to_owned()]);
        assert!(dto.trusted);
        assert_eq!(
            discovery_event_dto(&DiscoveryEvent::Lost("x".into()), |_| true),
            DiscoveryEventDto::Lost {
                device_id: "x".into()
            }
        );

        let key = [9u8; 32];
        let uri =
            hfa_proto::PairingUri::new("192.168.1.5", 47810, key, "dG9rZW4", "Desk").expect("uri");
        let parsed: hfa_proto::PairingUri = uri.to_string().parse().expect("parse");
        let dto = pairing_uri_dto(&parsed);
        assert_eq!(dto.host, "192.168.1.5");
        assert_eq!(dto.port, 47810);
        assert_eq!(
            crate::hub_target::decode_key(&dto.hub_id).expect("key"),
            key
        );
        assert_eq!(dto.hub_device_id, hfa_proto::fingerprint(&key));
        assert_eq!(dto.token, "dG9rZW4");
        assert_eq!(dto.name, "Desk");

        let info = PairingInfo {
            pin: "123456".into(),
            token: "tok".into(),
            uri: "hfa://pair?x".into(),
            expires_at_unix: u64::MAX,
        };
        let dto = pairing_info_dto(&info);
        assert_eq!(dto.expires_at_unix, i64::MAX, "saturates");
        assert_eq!(dto.pin, "123456");

        let peer = TrustedPeer {
            device_id: "id".into(),
            name: "Phone".into(),
            public_key: [1; 32],
            paired_at: 1_700_000_000,
        };
        assert_eq!(
            trusted_peer_dto(&peer),
            TrustedPeerDto {
                device_id: "id".into(),
                name: "Phone".into(),
                paired_at_unix: 1_700_000_000
            }
        );
    }

    #[test]
    fn capabilities_mapping() {
        let dto = capabilities_dto(Capabilities {
            system_mix: true,
            per_app: false,
            mutes_local_output: true,
            notes: "n".into(),
        });
        assert!(dto.system_mix && !dto.per_app && dto.mutes_local_output);
        assert_eq!(dto.notes, "n");
        assert_eq!(
            dto.external_only,
            cfg!(any(target_os = "android", target_os = "ios"))
        );
    }
}
