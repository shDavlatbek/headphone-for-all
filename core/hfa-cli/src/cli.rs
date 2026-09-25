//! The `hfa` command tree (clap derive). Every argument is parsed into a typed value.

use std::fmt;
use std::net::IpAddr;
use std::path::PathBuf;
use std::str::FromStr;

use clap::{Args, Parser, Subcommand};
use hfa_capture::{CaptureTarget, OutputTarget};
use hfa_proto::PairingUri;

/// headphone-for-all: play audio from several devices at the same time in one headphone.
#[derive(Debug, Parser)]
#[command(name = "hfa", version, about, propagate_version = true)]
pub struct Cli {
    /// Directory for settings, identity and trusted peers (default: the platform data dir).
    #[arg(long, global = true, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,

    /// More logging (-v: debug, -vv: trace). `RUST_LOG` overrides it.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level commands.
// Parsed once at startup; boxing the larger variants would only add noise.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run a hub: receive streams from senders and play the mix.
    Hub(HubArgs),
    /// Capture this device's audio and stream it to a hub.
    Send(SendArgs),
    /// List hubs found on the local network (mDNS).
    Discover(DiscoverArgs),
    /// Show output devices, capturable apps and capture capabilities.
    Devices,
    /// Manage trusted (paired) devices.
    Trust {
        /// Trust store command.
        #[command(subcommand)]
        command: TrustCommand,
    },
    /// Run an in-process hub + tone sender over localhost and check the result.
    Selftest(SelftestArgs),
}

/// `hfa hub`.
#[derive(Debug, Args)]
pub struct HubArgs {
    /// TCP control / UDP media port (default: settings, 47810).
    #[arg(long)]
    pub port: Option<u16>,

    /// Output: `default`, `device:<name>`, `wav:<path>` or `null` (default: settings).
    #[arg(long, value_name = "TARGET")]
    pub out: Option<OutputTarget>,

    /// Do not advertise the hub over mDNS.
    #[arg(long)]
    pub no_mdns: bool,

    /// Open a pairing window at start and print the PIN and pairing URI.
    #[arg(long)]
    pub pair: bool,
}

/// `hfa send`.
#[derive(Debug, Args)]
pub struct SendArgs {
    /// Where the hub is. At least one of `--to`, `--hub` or `--uri` is required; `--to` /
    /// `--hub` override the host and port of `--uri` (the URI still supplies the hub key and
    /// token).
    #[command(flatten)]
    pub dest: SendDestination,

    /// Pairing secret, needed only the first time.
    #[command(flatten)]
    pub auth: SendAuth,

    /// What to capture: `system`, `system-excl`, `tone:<hz>`, `wav:<path>` or `pid:<n>`.
    #[arg(long, value_name = "SOURCE", default_value = "system")]
    pub source: CaptureTarget,

    /// Opus bitrate in bits per second (default: settings, 128000).
    #[arg(long, value_parser = clap::value_parser!(u32).range(6_000..=510_000))]
    pub bitrate: Option<u32>,

    /// Opus frame duration in ms: 10 or 20 (default: settings, 10).
    #[arg(long, value_parser = parse_frame_ms)]
    pub frame_ms: Option<u32>,

    /// Label shown on the hub (default: derived from the source).
    #[arg(long)]
    pub label: Option<String>,
}

/// Mutually exclusive hub selectors of `hfa send`. One of them is required unless `--uri`
/// is given.
#[derive(Debug, Args)]
#[group(required = false, multiple = false)]
pub struct SendDestination {
    /// Hub address: `host`, `host:port`, `ip`, `ip:port` or `[ipv6]:port`.
    #[arg(
        long,
        value_name = "HOST[:PORT]",
        required_unless_present_any = ["hub", "uri"]
    )]
    pub to: Option<HostPort>,

    /// Hub device id or name, resolved through mDNS.
    #[arg(long, value_name = "NAME-OR-ID")]
    pub hub: Option<String>,
}

/// Mutually exclusive pairing secrets of `hfa send`.
#[derive(Debug, Args)]
#[group(required = false, multiple = false)]
pub struct SendAuth {
    /// The 6-digit PIN shown by the hub.
    #[arg(long, value_parser = parse_pin)]
    pub pin: Option<String>,

    /// The `hfa://pair?...` URI shown by the hub (contains host, port, key and token).
    #[arg(long, value_name = "HFA-URI")]
    pub uri: Option<PairingUri>,
}

/// `hfa discover`.
#[derive(Debug, Args)]
pub struct DiscoverArgs {
    /// How long to browse, in seconds.
    #[arg(long, default_value_t = 3)]
    pub timeout: u64,
}

/// `hfa trust ...`.
#[derive(Debug, Subcommand)]
pub enum TrustCommand {
    /// List trusted devices.
    List,
    /// Forget a trusted device.
    Remove {
        /// Device id (e.g. `ab12-cd34-ef56-7890`).
        device_id: String,
    },
}

/// `hfa selftest`.
#[derive(Debug, Args)]
pub struct SelftestArgs {
    /// Test duration in seconds.
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u32).range(1..=3600))]
    pub seconds: u32,

    /// Simulated packet loss in percent (0-100).
    #[arg(long, default_value_t = 0.0, value_parser = parse_percent)]
    pub loss: f32,

    /// Simulated network jitter in ms (uniform 0..=N extra delay per packet).
    #[arg(long, default_value_t = 0)]
    pub jitter: u32,
}

/// A `host[:port]` pair; the port defaults to [`hfa_proto::DEFAULT_PORT`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPort {
    /// Host name or IP address (IPv6 without brackets).
    pub host: String,
    /// TCP port.
    pub port: u16,
}

impl FromStr for HostPort {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let parse_port = |p: &str| {
            p.parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| format!("invalid port {p:?}"))
        };
        if s.is_empty() {
            return Err("empty host".to_owned());
        }
        if let Some(rest) = s.strip_prefix('[') {
            // [ipv6] or [ipv6]:port
            let (host, after) = rest
                .split_once(']')
                .ok_or_else(|| format!("missing ']' in {s:?}"))?;
            host.parse::<std::net::Ipv6Addr>()
                .map_err(|_| format!("invalid IPv6 address {host:?}"))?;
            let port = match after {
                "" => hfa_proto::DEFAULT_PORT,
                p => parse_port(
                    p.strip_prefix(':')
                        .ok_or_else(|| format!("bad address {s:?}"))?,
                )?,
            };
            return Ok(HostPort {
                host: host.to_owned(),
                port,
            });
        }
        if s.parse::<IpAddr>().is_ok() {
            // Bare IPv4 or IPv6 without port.
            return Ok(HostPort {
                host: s.to_owned(),
                port: hfa_proto::DEFAULT_PORT,
            });
        }
        match s.rsplit_once(':') {
            Some((host, _)) if host.contains(':') => {
                Err(format!("IPv6 address with port must use brackets: {s:?}"))
            }
            Some((host, port)) if !host.is_empty() => Ok(HostPort {
                host: host.to_owned(),
                port: parse_port(port)?,
            }),
            Some(_) => Err(format!("missing host in {s:?}")),
            None => Ok(HostPort {
                host: s.to_owned(),
                port: hfa_proto::DEFAULT_PORT,
            }),
        }
    }
}

impl fmt::Display for HostPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

fn parse_frame_ms(s: &str) -> Result<u32, String> {
    match s.trim() {
        "10" => Ok(10),
        "20" => Ok(20),
        other => Err(format!("frame size must be 10 or 20 ms, got {other:?}")),
    }
}

fn parse_pin(s: &str) -> Result<String, String> {
    let s = s.trim();
    if s.len() == 6 && s.bytes().all(|b| b.is_ascii_digit()) {
        Ok(s.to_owned())
    } else {
        Err("PIN must be exactly 6 digits".to_owned())
    }
}

fn parse_percent(s: &str) -> Result<f32, String> {
    match s.trim().parse::<f32>() {
        Ok(v) if (0.0..=100.0).contains(&v) => Ok(v),
        _ => Err(format!(
            "expected a percentage between 0 and 100, got {s:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn command_tree_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn host_port_forms() {
        let hp = |s: &str| s.parse::<HostPort>();
        assert_eq!(
            hp("desk.local").unwrap(),
            HostPort {
                host: "desk.local".into(),
                port: 47810
            }
        );
        assert_eq!(
            hp("10.0.0.2:5000").unwrap(),
            HostPort {
                host: "10.0.0.2".into(),
                port: 5000
            }
        );
        assert_eq!(
            hp("fe80::1").unwrap(),
            HostPort {
                host: "fe80::1".into(),
                port: 47810
            }
        );
        assert_eq!(
            hp("[fe80::1]:9").unwrap(),
            HostPort {
                host: "fe80::1".into(),
                port: 9
            }
        );
        assert_eq!(
            hp("[::1]").unwrap(),
            HostPort {
                host: "::1".into(),
                port: 47810
            }
        );
        for bad in [
            "",
            ":80",
            "host:",
            "host:0",
            "host:70000",
            "[::1",
            "[nope]:1",
            "fe80::1:x:",
        ] {
            assert!(hp(bad).is_err(), "{bad:?} should be rejected");
        }
        assert_eq!(hp("[::1]:5").unwrap().to_string(), "[::1]:5");
        assert_eq!(hp("a:5").unwrap().to_string(), "a:5");
    }

    #[test]
    fn parses_hub_command() {
        let cli = Cli::try_parse_from([
            "hfa",
            "hub",
            "--port",
            "5000",
            "--out",
            "wav:/tmp/x.wav",
            "--pair",
        ])
        .unwrap();
        match cli.command {
            Command::Hub(args) => {
                assert_eq!(args.port, Some(5000));
                assert_eq!(args.out, Some(OutputTarget::WavFile("/tmp/x.wav".into())));
                assert!(args.pair);
                assert!(!args.no_mdns);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parses_send_command() {
        let cli = Cli::try_parse_from([
            "hfa",
            "-vv",
            "send",
            "--to",
            "10.0.0.2:5000",
            "--pin",
            "123456",
            "--source",
            "tone:440",
            "--bitrate",
            "96000",
            "--frame-ms",
            "20",
        ])
        .unwrap();
        assert_eq!(cli.verbose, 2);
        match cli.command {
            Command::Send(args) => {
                assert_eq!(
                    args.dest.to,
                    Some(HostPort {
                        host: "10.0.0.2".into(),
                        port: 5000
                    })
                );
                assert_eq!(args.auth.pin.as_deref(), Some("123456"));
                assert_eq!(args.source, CaptureTarget::Tone { freq_hz: 440.0 });
                assert_eq!(args.bitrate, Some(96_000));
                assert_eq!(args.frame_ms, Some(20));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn rejects_invalid_send_args() {
        let bad: [&[&str]; 8] = [
            // No destination at all.
            &["hfa", "send"],
            &["hfa", "send", "--pin", "123456"],
            &["hfa", "send", "--source", "tone:440"],
            &["hfa", "send", "--to", "a", "--hub", "b"],
            &["hfa", "send", "--to", "a", "--pin", "12345"],
            &["hfa", "send", "--to", "a", "--frame-ms", "15"],
            &["hfa", "send", "--to", "a", "--bitrate", "100"],
            &["hfa", "send", "--to", "a", "--source", "speaker"],
        ];
        for args in bad {
            assert!(
                Cli::try_parse_from(args).is_err(),
                "{args:?} should be rejected"
            );
        }
    }

    #[test]
    fn send_by_hub_name_needs_no_address() {
        let cli = Cli::try_parse_from(["hfa", "send", "--hub", "Desk", "--pin", "000123"])
            .expect("--hub alone is a destination");
        match cli.command {
            Command::Send(args) => {
                assert_eq!(args.dest.hub.as_deref(), Some("Desk"));
                assert_eq!(args.dest.to, None);
                assert_eq!(args.source, CaptureTarget::SystemMix);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parses_other_commands() {
        let cli = Cli::try_parse_from(["hfa", "trust", "remove", "ab12-cd34-ef56-7890"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Trust { command: TrustCommand::Remove { ref device_id } } if device_id == "ab12-cd34-ef56-7890"
        ));
        let cli = Cli::try_parse_from([
            "hfa",
            "selftest",
            "--seconds",
            "3",
            "--loss",
            "5",
            "--jitter",
            "20",
        ])
        .unwrap();
        match cli.command {
            Command::Selftest(a) => {
                assert_eq!((a.seconds, a.loss, a.jitter), (3, 5.0, 20));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(Cli::try_parse_from(["hfa", "selftest", "--loss", "101"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["hfa", "devices"]).unwrap().command,
            Command::Devices
        ));
    }
}
