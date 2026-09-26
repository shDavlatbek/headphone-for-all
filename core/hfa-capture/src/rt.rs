//! Scheduling of the soft real-time audio threads: the hub's mixer, the sender's encoder and
//! the paced software sources and outputs (tone, WAV file, null).
//!
//! A plain thread gets whatever timer precision and priority the OS gives ordinary work. On a
//! loaded machine that is not enough for a thread that must wake every few milliseconds: a
//! 2.5 ms sleep can come back tens of milliseconds late, most of all on macOS, whose timer
//! coalescing stretches the sleeps of any process it rates as background work (a CI runner, a
//! helper launched by launchd). The audio pipeline hears that as dropouts. So every such
//! thread calls [`promote_current_thread`] once when it starts:
//!
//! - **macOS / iOS:** QoS class `USER_INTERACTIVE` (exempt from timer coalescing), then the
//!   Mach time-constraint (real-time) policy with the thread's period, the policy Core Audio's
//!   own I/O threads use. A thread that keeps overrunning its computation budget is demoted by
//!   the kernel's fail-safe, so it can never starve the system.
//! - **Windows:** the MMCSS "Pro Audio" task (real-time priority band) and a 1 ms system timer
//!   resolution (`timeBeginPeriod(1)`; the default 15.6 ms tick also governs
//!   `thread::park_timeout`) for as long as the returned guard lives.
//! - **Linux / Android:** best effort: a nice value of −10 for this thread. That needs
//!   `CAP_SYS_NICE` or a matching `RLIMIT_NICE` (Android grants app processes enough for its
//!   audio priorities); otherwise nothing changes. No rtkit / D-Bus.
//!
//! Everything fails soft: a thread that cannot be promoted simply runs as before. Nothing here
//! panics, allocates after the call, or logs from the audio loop (the one log line is emitted
//! by the call itself, before the thread's loop starts).

use std::marker::PhantomData;
use std::time::Duration;

/// What [`promote_current_thread`] achieved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Promotion {
    /// Nothing could be changed (unsupported platform, or not permitted).
    None,
    /// A higher priority / better timer class than a plain thread, but not real-time
    /// scheduling (macOS QoS only, Linux nice).
    Raised,
    /// Real-time scheduling (macOS time-constraint policy, Windows MMCSS).
    RealTime,
}

/// Keeps the current thread promoted; undoes what needs undoing (Windows: MMCSS task, timer
/// resolution) when dropped. Must be dropped on the thread that created it (it is `!Send`).
#[derive(Debug)]
#[must_use = "the promotion is undone when the guard is dropped"]
pub struct RtGuard {
    promotion: Promotion,
    #[cfg(windows)]
    mmcss: Option<windows::Win32::Foundation::HANDLE>,
    #[cfg(windows)]
    timer_period: bool,
    /// `!Send` + `!Sync`: the guard belongs to the thread it promoted.
    _thread_bound: PhantomData<*const ()>,
}

impl RtGuard {
    /// What the promotion achieved.
    pub fn promotion(&self) -> Promotion {
        self.promotion
    }

    fn new(promotion: Promotion) -> Self {
        Self {
            promotion,
            #[cfg(windows)]
            mmcss: None,
            #[cfg(windows)]
            timer_period: false,
            _thread_bound: PhantomData,
        }
    }
}

/// Promotes the calling thread for soft real-time audio work that wakes about every `period`
/// (see the module docs for what that means per OS). Returns a guard that keeps the promotion
/// until it is dropped; call it at the top of the thread's body and keep the guard for the
/// thread's lifetime. Never fails: [`RtGuard::promotion`] says what was achieved.
pub fn promote_current_thread(period: Duration) -> RtGuard {
    let guard = imp::promote(period.clamp(Duration::from_millis(1), Duration::from_millis(100)));
    tracing::debug!(
        thread = std::thread::current().name().unwrap_or("?"),
        promotion = ?guard.promotion,
        period_ms = period.as_secs_f64() * 1000.0,
        "audio thread scheduling"
    );
    guard
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod imp {
    use super::{Promotion, RtGuard};
    use std::time::Duration;

    pub(super) fn promote(period: Duration) -> RtGuard {
        let mut promotion = Promotion::None;
        // SAFETY: plain libpthread call on the current thread. It must come before the Mach
        // policy (it fails once the thread's scheduling policy was changed).
        let qos = unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
        };
        if qos == 0 {
            promotion = Promotion::Raised;
        }
        if time_constraint(period) {
            promotion = Promotion::RealTime;
        }
        RtGuard::new(promotion)
    }

    /// `mach_timebase_info_data_t` (`<mach/mach_time.h>`), declared here because the `libc`
    /// binding is deprecated in favour of the `mach2` crate.
    #[repr(C)]
    struct MachTimebaseInfo {
        numer: u32,
        denom: u32,
    }

    extern "C" {
        /// `<mach/mach_time.h>` (libSystem): the ratio of Mach absolute time units to ns.
        fn mach_timebase_info(info: *mut MachTimebaseInfo) -> libc::c_int;
    }

    /// The Mach time-constraint policy: the thread needs up to half of every `period` of CPU,
    /// delivered within 90 % of it (preemptible). The kernel only accepts computations of
    /// 50 µs ..= 50 ms, which the caller's clamp to 1..=100 ms periods respects.
    fn time_constraint(period: Duration) -> bool {
        let mut timebase = MachTimebaseInfo { numer: 0, denom: 0 };
        // SAFETY: writes the timebase into a valid local.
        if unsafe { mach_timebase_info(&mut timebase) } != 0
            || timebase.numer == 0
            || timebase.denom == 0
        {
            return false;
        }
        // Mach absolute time units = ns · denom / numer.
        let to_abs = |d: Duration| -> u32 {
            let abs = d.as_nanos() * u128::from(timebase.denom) / u128::from(timebase.numer);
            u32::try_from(abs).unwrap_or(u32::MAX)
        };
        let mut policy = libc::thread_time_constraint_policy {
            period: to_abs(period),
            computation: to_abs(period / 2),
            constraint: to_abs(period * 9 / 10),
            preemptible: 1,
        };
        // SAFETY: `pthread_mach_thread_np` returns the current thread's port without adding a
        // reference; `policy` is a valid THREAD_TIME_CONSTRAINT_POLICY record of the given
        // count.
        let status = unsafe {
            libc::thread_policy_set(
                libc::pthread_mach_thread_np(libc::pthread_self()),
                libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
                std::ptr::addr_of_mut!(policy).cast(),
                libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT,
            )
        };
        status == libc::KERN_SUCCESS
    }
}

#[cfg(windows)]
mod imp {
    use super::{Promotion, RtGuard};
    use std::time::Duration;
    use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod, TIMERR_NOERROR};
    use windows::Win32::System::Threading::{
        AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
    };

    pub(super) fn promote(_period: Duration) -> RtGuard {
        let mut guard = RtGuard::new(Promotion::None);
        // SAFETY: plain winmm call; undone by exactly one timeEndPeriod in `Drop`.
        if unsafe { timeBeginPeriod(1) } == TIMERR_NOERROR {
            guard.timer_period = true;
            guard.promotion = Promotion::Raised;
        }
        let mut task_index = 0u32;
        // SAFETY: static task name; `task_index` is a valid out-pointer. The handle is
        // reverted on this thread in `Drop` (the guard is `!Send`).
        if let Ok(handle) = unsafe {
            AvSetMmThreadCharacteristicsW(windows::core::w!("Pro Audio"), &mut task_index)
        } {
            if !handle.is_invalid() {
                guard.mmcss = Some(handle);
                guard.promotion = Promotion::RealTime;
            }
        }
        guard
    }

    impl Drop for RtGuard {
        fn drop(&mut self) {
            if let Some(handle) = self.mmcss.take() {
                // SAFETY: `handle` came from AvSetMmThreadCharacteristicsW on this thread.
                let _ = unsafe { AvRevertMmThreadCharacteristics(handle) };
            }
            if std::mem::take(&mut self.timer_period) {
                // SAFETY: pairs the successful timeBeginPeriod(1) of `promote`.
                let _ = unsafe { timeEndPeriod(1) };
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod imp {
    use super::{Promotion, RtGuard};
    use std::time::Duration;

    /// Nice value asked for (Android's `THREAD_PRIORITY_AUDIO` is −16; −10 stays below the
    /// usual `RLIMIT_NICE` grants of audio groups).
    const NICE: libc::c_int = -10;

    pub(super) fn promote(_period: Duration) -> RtGuard {
        // SAFETY: plain syscalls on the current thread (`setpriority` with a thread id changes
        // only that thread on Linux).
        let raised = unsafe {
            let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
            libc::setpriority(libc::PRIO_PROCESS, tid, NICE) == 0
        };
        RtGuard::new(if raised {
            Promotion::Raised
        } else {
            Promotion::None
        })
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    windows,
    target_os = "linux",
    target_os = "android"
)))]
mod imp {
    use super::{Promotion, RtGuard};
    use std::time::Duration;

    pub(super) fn promote(_period: Duration) -> RtGuard {
        RtGuard::new(Promotion::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Instant;

    #[test]
    fn promotion_never_fails_and_can_be_repeated() {
        let first = promote_current_thread(Duration::from_millis(10));
        // Extreme periods are clamped, not rejected.
        let second = promote_current_thread(Duration::ZERO);
        let third = promote_current_thread(Duration::from_secs(3600));
        assert!(second.promotion() <= Promotion::RealTime);
        drop(third);
        drop(second);
        drop(first);
        #[cfg(any(target_os = "macos", target_os = "ios", windows))]
        {
            // These OSes let any process promote its own threads.
            let guard = std::thread::spawn(|| {
                promote_current_thread(Duration::from_millis(10)).promotion()
            })
            .join()
            .expect("thread");
            assert!(guard > Promotion::None, "{guard:?}");
        }
    }

    /// Overshoot percentiles (µs) of `count` sleeps of `period` on the current thread.
    fn overshoot(period: Duration, count: usize) -> [u128; 4] {
        let mut late: Vec<u128> = (0..count)
            .map(|_| {
                let t0 = Instant::now();
                std::thread::sleep(period);
                t0.elapsed().saturating_sub(period).as_micros()
            })
            .collect();
        late.sort_unstable();
        let pct = |p: usize| late[(late.len() - 1) * p / 100];
        [pct(50), pct(90), pct(99), late[late.len() - 1]]
    }

    /// Diagnostic only (never fails): how late `thread::sleep` wakes up on this machine, for a
    /// plain thread and for a promoted one. Written straight to stderr, which the test
    /// harness does not capture, so every CI log shows the timer quality of its runner.
    #[test]
    fn timer_quality_report() {
        let run = |promote: bool| {
            std::thread::spawn(move || {
                let guard = promote.then(|| promote_current_thread(Duration::from_millis(10)));
                let promotion = guard.as_ref().map_or(Promotion::None, RtGuard::promotion);
                let short = overshoot(Duration::from_micros(2500), 200);
                let tick = overshoot(Duration::from_millis(10), 50);
                (promotion, short, tick)
            })
            .join()
            .expect("measurement thread")
        };
        let mut report = format!(
            "\ntimer quality on {} ({} CPUs): sleep overshoot in µs, p50 / p90 / p99 / max\n",
            std::env::consts::OS,
            std::thread::available_parallelism().map_or(0, usize::from)
        );
        for promote in [false, true] {
            let (promotion, short, tick) = run(promote);
            let label = if promote { "promoted" } else { "plain" };
            report += &format!(
                "  {label:<8} thread ({promotion:?}): 2.5 ms sleeps {short:?}, 10 ms sleeps {tick:?}\n"
            );
        }
        let _ = std::io::stderr().write_all(report.as_bytes());
    }
}
