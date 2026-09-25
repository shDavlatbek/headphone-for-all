//! Network impairment for tests and `hfa selftest`: a one-way UDP relay that drops and
//! delays datagrams.
//!
//! Point a sender's media at [`UdpImpairProxy::local_addr`] (on the hub:
//! [`crate::HubHandle::set_media_port_override`] with the proxy's port) and the proxy forwards
//! every datagram to the real hub port, dropping [`ImpairConfig::loss_pct`] percent of them
//! at random and delaying each one by a random `0..=jitter_ms` ms, which also **reorders**
//! datagrams whenever the delay exceeds the packet interval. Deterministic for a given seed.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::Result;

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

/// A running one-way UDP impairment relay (see the module docs).
#[derive(Debug)]
pub struct UdpImpairProxy {
    local_addr: SocketAddr,
    counters: Arc<Counters>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
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
    /// [`crate::CoreError::Io`] if a socket cannot be bound.
    pub async fn start(target: SocketAddr, config: ImpairConfig) -> Result<UdpImpairProxy> {
        let bind_ip: IpAddr = match target.ip() {
            ip if ip.is_loopback() => ip,
            IpAddr::V4(_) => Ipv4Addr::UNSPECIFIED.into(),
            IpAddr::V6(_) => Ipv6Addr::UNSPECIFIED.into(),
        };
        let socket = Arc::new(UdpSocket::bind((bind_ip, 0)).await?);
        let local_addr = socket.local_addr()?;
        let counters = Arc::new(Counters::default());
        let (stop, mut stopped) = watch::channel(false);
        let task_counters = Arc::clone(&counters);
        let task = tokio::spawn(async move {
            let mut rng = SplitMix(config.seed);
            let mut buf = vec![0u8; 65_536];
            let loss = f64::from(config.loss_pct.clamp(0.0, 100.0)) / 100.0;
            loop {
                let received = tokio::select! {
                    r = socket.recv_from(&mut buf) => r,
                    _ = stopped.wait_for(|s| *s) => break,
                };
                let Ok((n, _)) = received else {
                    continue;
                };
                task_counters.received.fetch_add(1, Ordering::Relaxed);
                if rng.next_f64() < loss {
                    task_counters.dropped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let datagram = buf[..n].to_vec();
                let delay_us = if config.jitter_ms == 0 {
                    0
                } else {
                    (rng.next_f64() * f64::from(config.jitter_ms) * 1000.0) as u64
                };
                let socket = Arc::clone(&socket);
                let counters = Arc::clone(&task_counters);
                let forward = async move {
                    if delay_us > 0 {
                        tokio::time::sleep(Duration::from_micros(delay_us)).await;
                    }
                    if socket.send_to(&datagram, target).await.is_ok() {
                        counters.forwarded.fetch_add(1, Ordering::Relaxed);
                    }
                };
                if delay_us == 0 {
                    forward.await;
                } else {
                    tokio::spawn(forward);
                }
            }
        });
        Ok(UdpImpairProxy {
            local_addr,
            counters,
            stop,
            task,
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

    /// Stops relaying (datagrams still being delayed may be delivered afterwards).
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
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
