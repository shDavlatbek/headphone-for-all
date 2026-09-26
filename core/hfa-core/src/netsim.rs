//! Network impairment for tests and `hfa selftest`: a one-way UDP relay that drops and
//! delays datagrams.
//!
//! Point a sender's media at [`UdpImpairProxy::local_addr`] (on the hub:
//! [`crate::HubHandle::set_media_port_override`] with the proxy's port) and the proxy forwards
//! every datagram to the real hub port, dropping [`ImpairConfig::loss_pct`] percent of them
//! at random and delaying each one by a random `0..=jitter_ms` ms, which also **reorders**
//! datagrams whenever the delay exceeds the packet interval. Deterministic for a given seed.
//!
//! The relay runs on its own OS thread (`hfa-netsim`), promoted like the hub's media receive
//! thread ([`hfa_capture::rt::promote_current_thread`]) on a blocking socket, with the delayed
//! datagrams in a timer queue. It used to be a tokio task: on a loaded CI runner a
//! normal-priority tokio worker wakes up tens of milliseconds late, and the relay then held
//! back **every** sender's datagrams at once and released them in a burst — a network stall
//! the configuration never asked for (every stream underran together, then the hub had to cut
//! the burst's excess latency).

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::{CoreError, Result};

/// The relay thread checks its stop flag at least this often.
const POLL: Duration = Duration::from_millis(20);
/// Period the relay thread is promoted for (the senders' packet interval).
const RELAY_PERIOD: Duration = Duration::from_millis(10);
/// Receive buffer: room for any UDP datagram.
const RECV_BUFFER: usize = 65_536;

/// How the proxy impairs the traffic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImpairConfig {
    /// Datagrams dropped at random, in percent (0..=100).
    pub loss_pct: f32,
    /// Largest random extra delay per datagram in ms (0 = forward immediately).
    pub jitter_ms: u32,
    /// Seed of the pseudo-random generator.
    pub seed: u64,
}

impl Default for ImpairConfig {
    /// No loss, no jitter.
    fn default() -> Self {
        Self {
            loss_pct: 0.0,
            jitter_ms: 0,
            seed: 0x5eed,
        }
    }
}

/// Counters of an [`UdpImpairProxy`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProxyStats {
    /// Datagrams received.
    pub received: u64,
    /// Datagrams dropped on purpose.
    pub dropped: u64,
    /// Datagrams forwarded.
    pub forwarded: u64,
}

#[derive(Debug, Default)]
struct Counters {
    received: AtomicU64,
    dropped: AtomicU64,
    forwarded: AtomicU64,
}

/// A running one-way UDP impairment relay (see the module docs). Dropping it stops the relay
/// thread in the background.
#[derive(Debug)]
pub struct UdpImpairProxy {
    local_addr: SocketAddr,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// SplitMix64: tiny, deterministic, good enough for simulations.
#[derive(Debug, Clone)]
struct SplitMix(u64);

impl SplitMix {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

impl UdpImpairProxy {
    /// Binds a socket on the loopback (or unspecified) address of `target`'s family and starts
    /// relaying to `target`.
    ///
    /// # Errors
    /// [`crate::CoreError::Io`] if a socket cannot be bound or the thread cannot start.
    pub async fn start(target: SocketAddr, config: ImpairConfig) -> Result<UdpImpairProxy> {
        let bind_ip: IpAddr = match target.ip() {
            ip if ip.is_loopback() => ip,
            IpAddr::V4(_) => Ipv4Addr::UNSPECIFIED.into(),
            IpAddr::V6(_) => Ipv6Addr::UNSPECIFIED.into(),
        };
        let socket = UdpSocket::bind((bind_ip, 0))?;
        // Like the real media sockets: the relay must not add drops of its own.
        crate::media::enlarge_buffers(socket2::SockRef::from(&socket));
        socket.set_read_timeout(Some(POLL))?;
        let local_addr = socket.local_addr()?;
        let counters = Arc::new(Counters::default());
        let stop = Arc::new(AtomicBool::new(false));
        let relay = Relay {
            socket,
            target,
            config,
            counters: Arc::clone(&counters),
            stop: Arc::clone(&stop),
        };
        let thread = std::thread::Builder::new()
            .name("hfa-netsim".into())
            .spawn(move || {
                let _rt = hfa_capture::rt::promote_current_thread(RELAY_PERIOD);
                relay.run();
            })
            .map_err(|e| CoreError::Io(format!("cannot start the netsim relay thread: {e}")))?;
        Ok(UdpImpairProxy {
            local_addr,
            counters,
            stop,
            thread: Some(thread),
        })
    }

    /// Where to send datagrams.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Counters so far.
    pub fn stats(&self) -> ProxyStats {
        ProxyStats {
            received: self.counters.received.load(Ordering::Relaxed),
            dropped: self.counters.dropped.load(Ordering::Relaxed),
            forwarded: self.counters.forwarded.load(Ordering::Relaxed),
        }
    }

    /// Stops relaying and waits for the relay thread (datagrams still being delayed are
    /// discarded).
    pub async fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
    }
}

impl Drop for UdpImpairProxy {
    fn drop(&mut self) {
        // The thread notices within `POLL` and exits on its own.
        self.stop.store(true, Ordering::Release);
    }
}

/// The relay thread's state.
struct Relay {
    socket: UdpSocket,
    target: SocketAddr,
    config: ImpairConfig,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
}

impl Relay {
    fn run(self) {
        let mut rng = SplitMix(self.config.seed);
        let mut buf = vec![0u8; RECV_BUFFER];
        let loss = f64::from(self.config.loss_pct.clamp(0.0, 100.0)) / 100.0;
        // Delayed datagrams: (due, arrival order, datagram), earliest first.
        let mut delayed: BinaryHeap<Reverse<(Instant, u64, Vec<u8>)>> = BinaryHeap::new();
        let mut order = 0u64;
        let mut errors_in_a_row = 0u32;
        while !self.stop.load(Ordering::Acquire) {
            let now = Instant::now();
            while delayed
                .peek()
                .is_some_and(|Reverse((due, _, _))| *due <= now)
            {
                if let Some(Reverse((_, _, datagram))) = delayed.pop() {
                    self.forward(&datagram);
                }
            }
            // Wait for the next datagram, at most until the next delayed one is due.
            let wait = delayed.peek().map_or(POLL, |Reverse((due, _, _))| {
                due.saturating_duration_since(now)
                    .clamp(Duration::from_micros(100), POLL)
            });
            if self.socket.set_read_timeout(Some(wait)).is_err() {
                std::thread::sleep(Duration::from_millis(1));
            }
            let n = match self.socket.recv_from(&mut buf) {
                Ok((n, _)) => {
                    errors_in_a_row = 0;
                    n
                }
                // The read timeout: look at the queue and the stop flag again.
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue
                }
                // A per-datagram error (Windows reports an earlier ICMP "port unreachable"
                // as ConnectionReset); only a long run of them earns a pause, so a broken
                // socket cannot make this promoted thread spin.
                Err(_) => {
                    errors_in_a_row = errors_in_a_row.saturating_add(1);
                    if errors_in_a_row > 100 {
                        errors_in_a_row = 0;
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    continue;
                }
            };
            self.counters.received.fetch_add(1, Ordering::Relaxed);
            if rng.next_f64() < loss {
                self.counters.dropped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if self.config.jitter_ms == 0 {
                self.forward(&buf[..n]);
                continue;
            }
            let delay_us = (rng.next_f64() * f64::from(self.config.jitter_ms) * 1000.0) as u64;
            let due = Instant::now() + Duration::from_micros(delay_us);
            delayed.push(Reverse((due, order, buf[..n].to_vec())));
            order += 1;
        }
    }

    fn forward(&self, datagram: &[u8]) {
        if self.socket.send_to(datagram, self.target).is_ok() {
            self.counters.forwarded.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix_is_uniform_enough() {
        let mut rng = SplitMix(1);
        let n = 100_000;
        let below: usize = (0..n).filter(|_| rng.next_f64() < 0.05).count();
        let pct = below as f64 * 100.0 / n as f64;
        assert!((4.7..5.3).contains(&pct), "{pct}");
    }

    #[tokio::test]
    async fn drops_and_delays_datagrams() {
        use tokio::net::UdpSocket;
        let sink = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
        let proxy = UdpImpairProxy::start(
            sink.local_addr().expect("addr"),
            ImpairConfig {
                loss_pct: 20.0,
                jitter_ms: 5,
                seed: 7,
            },
        )
        .await
        .expect("proxy");
        let reader = tokio::spawn(async move {
            let mut got = Vec::new();
            let mut buf = [0u8; 16];
            while let Ok(Ok((n, _))) =
                tokio::time::timeout(Duration::from_millis(500), sink.recv_from(&mut buf)).await
            {
                assert_eq!(n, 4);
                got.push(u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]));
            }
            got
        });
        let src = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
        for i in 0..500u32 {
            if i % 20 == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            src.send_to(&i.to_be_bytes(), proxy.local_addr())
                .await
                .expect("send");
        }
        let got = reader.await.expect("reader");
        let stats = proxy.stats();
        proxy.stop().await;
        assert_eq!(stats.received, 500);
        assert_eq!(stats.forwarded + stats.dropped, 500);
        assert_eq!(got.len() as u64, stats.forwarded);
        assert!((60..=140).contains(&stats.dropped), "{stats:?}");
        assert!(
            got.windows(2).any(|w| w[1] < w[0]),
            "random delays reorder datagrams"
        );
    }
}
