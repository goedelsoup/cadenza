//! Daemon supervisor with restart-on-crash and exponential backoff.
//!
//! This module owns the lifecycle of the `cadenza-daemon` child process for
//! the bundled Tauri shell. It is split out from `main.rs` so it can be
//! exercised by unit tests against a synthetic spawner — the real binary is
//! never invoked from the test suite.
//!
//! ## State machine
//!
//! ```text
//!     NotStarted
//!         │ start()
//!         ▼
//!      Running ──── shutdown() ───▶ NotStarted
//!         │
//!         │ tick() observes child exited
//!         ▼
//!     Restarting ──── tick() at retry_at, spawn ok ───▶ Running
//!         │                                              │
//!         │ rate-limit exceeded                          │ uptime ≥ reset_after
//!         ▼                                              ▼
//!      Failing                                    backoff resets to initial
//! ```
//!
//! - `start()` performs the initial spawn (does NOT count against the
//!   rate limit; only crashes do).
//! - `tick()` is called from the shell's 1s polling thread. Each call:
//!     1. If `Running`, polls `Child::try_wait`. On unexpected exit,
//!        records the exit code, increments the restart history, and
//!        transitions to `Restarting` with the next backoff.
//!     2. If `Running` and uptime ≥ `reset_after`, resets `next_backoff`
//!        to `initial_backoff` so the *next* crash starts fresh.
//!     3. If `Restarting` and the wall clock has reached `retry_at`,
//!        attempts a respawn. Spawn failures count as restart attempts.
//!     4. If the rate limit (`rate_limit_max` restarts inside
//!        `rate_limit_window`) is exceeded, transitions to `Failing` and
//!        stops trying.
//! - `shutdown()` is the user-initiated teardown path: kill, reap, set to
//!   `NotStarted`. Subsequent `tick()` calls are no-ops until `start()`.
//!
//! The supervisor holds a single `Mutex<Inner>`. All public methods take
//! `&self` and lock that mutex; the polling thread, the tray menu's quit
//! handler, and the Tauri window-event handler can all call into it
//! without coordinating ownership.

use std::collections::VecDeque;
use std::io;
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Constructs a fresh `Child`. The default impl spawns the located daemon
/// binary; tests pass a closure that runs `sh -c "exit 1"` instead.
///
/// Returning `Err` is treated as a *failed restart attempt* — it counts
/// against the rate limit and the supervisor will keep trying until the
/// limit is hit, at which point it transitions to `Failing`.
pub type Spawner = Arc<dyn Fn() -> io::Result<Child> + Send + Sync>;

/// Tunable thresholds for the restart loop. The `Default` impl matches the
/// values described in the Phase 5c spec; tests substitute much smaller
/// durations so the suite stays fast.
#[derive(Clone, Debug)]
pub struct SupervisorConfig {
    /// Backoff for the *first* crash after a fresh start (or after the
    /// `reset_after` uptime threshold has been crossed).
    pub initial_backoff:   Duration,
    /// Cap on `next_backoff` after repeated doubling.
    pub max_backoff:       Duration,
    /// Continuous uptime required before `next_backoff` resets to
    /// `initial_backoff`. Independent of the rate limit window.
    pub reset_after:       Duration,
    /// Maximum restart attempts permitted inside `rate_limit_window`.
    /// Once this is exceeded the supervisor enters `Failing`.
    pub rate_limit_max:    u32,
    /// Sliding window over which `rate_limit_max` is evaluated.
    pub rate_limit_window: Duration,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            initial_backoff:   Duration::from_secs(1),
            max_backoff:       Duration::from_secs(30),
            reset_after:       Duration::from_secs(60),
            rate_limit_max:    5,
            rate_limit_window: Duration::from_secs(5 * 60),
        }
    }
}

/// Externally observable state. The tray label is rendered from this via
/// [`DaemonSupervisor::status_label`]; tests inspect it via
/// [`DaemonSupervisor::state`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonState {
    /// Initial state, and the state after `shutdown()`.
    NotStarted,
    /// Healthy. `pid` is informational; only the supervisor's stored
    /// `Child` is authoritative.
    Running { pid: u32 },
    /// Last attempt crashed; we're waiting out the backoff before the
    /// next respawn. `attempt` is 1-indexed and counts only crashes
    /// (initial spawn is not attempt 0). `backoff` is the delay we are
    /// currently serving (already added to `retry_at`).
    Restarting {
        attempt:        u32,
        backoff:        Duration,
        last_exit_code: Option<i32>,
        retry_at:       Instant,
    },
    /// Rate limit exceeded. The supervisor will not auto-respawn from
    /// this state; the user can re-arm via an explicit `start()`.
    Failing { last_exit_code: Option<i32> },
}

impl DaemonState {
    fn label(&self) -> String {
        match self {
            DaemonState::NotStarted        => "not started".to_string(),
            DaemonState::Running { pid }   => format!("running (pid {pid})"),
            DaemonState::Restarting { attempt, backoff, .. } => {
                format!("restarting (attempt {attempt}, backoff {}s)", backoff.as_secs().max(1))
            }
            DaemonState::Failing { last_exit_code } => match last_exit_code {
                Some(code) => format!("failing repeatedly (last exit {code})"),
                None       => "failing repeatedly".to_string(),
            },
        }
    }
}

struct Inner {
    state:           DaemonState,
    child:           Option<Child>,
    /// Wall-clock instant the *current* `Running` child was spawned.
    /// Used to evaluate `reset_after`. `None` outside `Running`.
    started_at:      Option<Instant>,
    /// Sliding window of recent restart-attempt timestamps. Pruned on
    /// every push. Used for the rate limit.
    restart_history: VecDeque<Instant>,
    /// The delay the *next* crash will use. Doubles after each crash,
    /// capped at `max_backoff`, reset to `initial_backoff` after a
    /// successful uptime of `reset_after`.
    next_backoff:    Duration,
}

pub struct DaemonSupervisor {
    inner:   Mutex<Inner>,
    spawner: Spawner,
    config:  SupervisorConfig,
}

impl DaemonSupervisor {
    pub fn new(spawner: Spawner, config: SupervisorConfig) -> Self {
        let next_backoff = config.initial_backoff;
        Self {
            inner: Mutex::new(Inner {
                state:           DaemonState::NotStarted,
                child:           None,
                started_at:      None,
                restart_history: VecDeque::new(),
                next_backoff,
            }),
            spawner,
            config,
        }
    }

    /// Initial spawn. Does *not* count against the rate limit — only
    /// crash-driven restarts do. Returns `Ok` even if the spawn failed,
    /// because the supervisor's state already reflects the outcome
    /// (either `Running` or, on a spawn error, `Restarting` with the
    /// initial backoff). Callers (i.e. `setup()`) only need it to know
    /// whether to log a warning.
    pub fn start(&self) -> bool {
        let mut g = self.lock();
        // From a clean slate, reset history + backoff.
        g.restart_history.clear();
        g.next_backoff = self.config.initial_backoff;
        match (self.spawner)() {
            Ok(child) => {
                let pid = child.id();
                tracing::info!("cadenza-daemon spawned (pid {pid})");
                g.child      = Some(child);
                g.started_at = Some(Instant::now());
                g.state      = DaemonState::Running { pid };
                true
            }
            Err(e) => {
                tracing::warn!("initial cadenza-daemon spawn failed: {e}");
                // Treat the same as a crash so the user gets the same
                // restart-with-backoff behaviour even when the binary is
                // missing at startup. This *does* count against the rate
                // limit so a permanently-missing binary doesn't loop
                // forever.
                self.schedule_restart_locked(&mut g, None);
                false
            }
        }
    }

    /// Drive the state machine forward. The shell calls this once per
    /// second from the polling thread. Idempotent and cheap when nothing
    /// has changed.
    pub fn tick(&self) {
        let mut g = self.lock();
        match g.state.clone() {
            DaemonState::NotStarted | DaemonState::Failing { .. } => {
                // Terminal-ish; nothing to do until the user re-arms via
                // start().
            }
            DaemonState::Running { .. } => {
                // First, check if the child has exited.
                let exit = match g.child.as_mut() {
                    Some(child) => match child.try_wait() {
                        Ok(Some(status)) => Some(status.code()),
                        Ok(None)         => None,
                        Err(e) => {
                            // try_wait() errors are extremely rare and
                            // usually mean the OS lost track of the pid.
                            // Treat as an unknown-code exit.
                            tracing::warn!("try_wait failed on daemon child: {e}");
                            Some(None)
                        }
                    },
                    None => Some(None),
                };

                if let Some(code) = exit {
                    // Reap the corpse to free the zombie slot.
                    if let Some(mut child) = g.child.take() {
                        let _ = child.wait();
                    }
                    g.started_at = None;
                    tracing::warn!("cadenza-daemon exited unexpectedly (code {code:?}); scheduling restart");
                    self.schedule_restart_locked(&mut g, code);
                } else if let Some(started_at) = g.started_at {
                    // Still running. If we've been up long enough, reset
                    // the backoff so the *next* crash starts at the
                    // initial value again.
                    if started_at.elapsed() >= self.config.reset_after
                        && g.next_backoff != self.config.initial_backoff
                    {
                        g.next_backoff = self.config.initial_backoff;
                    }
                }
            }
            DaemonState::Restarting { retry_at, .. } => {
                if Instant::now() >= retry_at {
                    self.attempt_respawn_locked(&mut g);
                }
            }
        }
    }

    /// User-initiated teardown: kill the child, reap, return to
    /// `NotStarted`. Idempotent. Calling this from both the window
    /// `Destroyed` event and the global `RunEvent::Exit` is safe.
    pub fn shutdown(&self) {
        let mut g = self.lock();
        if let Some(mut child) = g.child.take() {
            let _ = child.kill();
            let _ = child.wait();
            tracing::info!("cadenza-daemon child reaped");
        }
        g.started_at = None;
        g.state      = DaemonState::NotStarted;
        // Don't clear restart_history — if the user re-arms via start()
        // we reset there. Leaving it here is harmless.
    }

    /// Snapshot of the current state. Cheap; tests use this to assert.
    /// Not used by `main.rs` directly (the tray label goes through
    /// `status_label` instead) but kept on the public surface so tests
    /// in this crate and any future code that wants programmatic access
    /// don't have to parse a string.
    #[allow(dead_code)]
    pub fn state(&self) -> DaemonState {
        self.lock().state.clone()
    }

    /// Tray label, e.g. `"daemon: running (pid 4321)"` or
    /// `"daemon: restarting (attempt 3, backoff 4s)"`.
    pub fn status_label(&self) -> String {
        format!("daemon: {}", self.lock().state.label())
    }

    // ── internals ────────────────────────────────────────────────────────

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // Mutex poison only happens if a previous holder panicked while
        // mutating Inner. The supervisor has no panicking call sites
        // we don't already control, so we recover by taking the inner
        // value rather than propagating the poison.
        match self.inner.lock() {
            Ok(g)  => g,
            Err(p) => p.into_inner(),
        }
    }

    /// Common path for "we just observed a crash; figure out the next
    /// step". Either schedules a `Restarting` state or transitions to
    /// `Failing` if the rate limit has been exceeded. Caller already
    /// holds the inner lock.
    fn schedule_restart_locked(&self, g: &mut Inner, last_exit_code: Option<i32>) {
        let now = Instant::now();

        // Prune restart_history outside the rate-limit window, then
        // record this attempt.
        let cutoff = now.checked_sub(self.config.rate_limit_window);
        if let Some(cutoff) = cutoff {
            while let Some(front) = g.restart_history.front() {
                if *front < cutoff { g.restart_history.pop_front(); } else { break; }
            }
        }
        g.restart_history.push_back(now);

        if g.restart_history.len() as u32 > self.config.rate_limit_max {
            tracing::error!(
                "cadenza-daemon failed {} times within {:?}; giving up",
                g.restart_history.len(),
                self.config.rate_limit_window,
            );
            g.state = DaemonState::Failing { last_exit_code };
            return;
        }

        let backoff = g.next_backoff;
        g.next_backoff = (g.next_backoff * 2).min(self.config.max_backoff);
        let attempt = g.restart_history.len() as u32;
        g.state = DaemonState::Restarting {
            attempt,
            backoff,
            last_exit_code,
            retry_at: now + backoff,
        };
    }

    /// Called from `tick()` when the `Restarting` deadline has passed.
    /// On a successful spawn, transitions to `Running`. On a spawn
    /// failure, treats the failure itself as a fresh crash and
    /// re-enters `schedule_restart_locked` (which doubles the backoff
    /// and re-checks the rate limit).
    fn attempt_respawn_locked(&self, g: &mut Inner) {
        // Pull last_exit_code so we can carry it through if respawn
        // fails — useful for the tray label.
        let prior_exit = match &g.state {
            DaemonState::Restarting { last_exit_code, .. } => *last_exit_code,
            _ => None,
        };
        match (self.spawner)() {
            Ok(child) => {
                let pid = child.id();
                tracing::info!("cadenza-daemon respawned (pid {pid})");
                g.child      = Some(child);
                g.started_at = Some(Instant::now());
                g.state      = DaemonState::Running { pid };
            }
            Err(e) => {
                tracing::warn!("respawn failed: {e}");
                self.schedule_restart_locked(g, prior_exit);
            }
        }
    }
}

// ─── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::thread::sleep;

    /// Tight config so the suite finishes in under a second.
    fn test_config() -> SupervisorConfig {
        SupervisorConfig {
            initial_backoff:   Duration::from_millis(50),
            max_backoff:       Duration::from_millis(400),
            reset_after:       Duration::from_millis(300),
            rate_limit_max:    5,
            rate_limit_window: Duration::from_secs(5),
        }
    }

    /// Spawner that runs `sh -c "exit <code>"` and exits immediately.
    /// Used to simulate a daemon that crashes the moment it boots so
    /// `tick()` can observe the corpse on the next poll.
    fn crash_immediately_spawner(code: i32) -> Spawner {
        Arc::new(move || {
            Command::new("sh")
                .arg("-c")
                .arg(format!("exit {code}"))
                .spawn()
        })
    }

    /// Spawner that sleeps `sleep_ms` ms then exits with `code`.
    /// Used to simulate a daemon that runs briefly, so we can observe
    /// the supervisor while it is in `Running`.
    fn sleep_then_exit_spawner(sleep_ms: u64, code: i32) -> Spawner {
        Arc::new(move || {
            Command::new("sh")
                .arg("-c")
                .arg(format!("sleep {}; exit {code}", sleep_ms as f64 / 1000.0))
                .spawn()
        })
    }

    /// Tick at a tight cadence until `pred` is true or we run out of
    /// budget. Real polling happens at 1s in production, but tests need
    /// it tighter so they don't sleep for seconds at a time. Returns
    /// `false` on timeout so the assertion site has a clean failure.
    fn tick_until(sup: &DaemonSupervisor, budget: Duration, mut pred: impl FnMut(&DaemonState) -> bool) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            sup.tick();
            if pred(&sup.state()) { return true; }
            if Instant::now() >= deadline { return false; }
            sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn start_transitions_to_running() {
        let sup = DaemonSupervisor::new(sleep_then_exit_spawner(500, 0), test_config());
        assert!(sup.start());
        assert!(matches!(sup.state(), DaemonState::Running { .. }));
        sup.shutdown();
    }

    #[test]
    fn detects_crash_and_enters_restarting() {
        let sup = DaemonSupervisor::new(crash_immediately_spawner(1), test_config());
        sup.start();
        // The child exits immediately. Tick a few times until the
        // supervisor observes the crash.
        let saw_restart = tick_until(&sup, Duration::from_millis(500), |s| {
            matches!(s, DaemonState::Restarting { .. })
        });
        assert!(saw_restart, "supervisor never observed the crash; state={:?}", sup.state());
        if let DaemonState::Restarting { attempt, last_exit_code, .. } = sup.state() {
            assert_eq!(attempt, 1);
            assert_eq!(last_exit_code, Some(1));
        }
        sup.shutdown();
    }

    #[test]
    fn backoff_doubles_across_successive_crashes() {
        // Spawner crashes immediately every time. We expect the
        // supervisor to ride the schedule 50 → 100 → 200 → 400 → 400.
        let sup = DaemonSupervisor::new(crash_immediately_spawner(2), SupervisorConfig {
            // Generous rate limit so we don't trip Failing during this test.
            rate_limit_max: 50,
            ..test_config()
        });
        sup.start();

        let mut observed: Vec<Duration> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        // Capture each distinct Restarting backoff value.
        let mut last_attempt: u32 = 0;
        while observed.len() < 5 && Instant::now() < deadline {
            sup.tick();
            if let DaemonState::Restarting { attempt, backoff, .. } = sup.state() {
                if attempt != last_attempt {
                    last_attempt = attempt;
                    observed.push(backoff);
                }
            }
            sleep(Duration::from_millis(5));
        }

        assert!(observed.len() >= 4, "only saw {} backoff values: {:?}", observed.len(), observed);
        assert_eq!(observed[0], Duration::from_millis(50));
        assert_eq!(observed[1], Duration::from_millis(100));
        assert_eq!(observed[2], Duration::from_millis(200));
        assert_eq!(observed[3], Duration::from_millis(400));
        if observed.len() >= 5 {
            // Capped.
            assert_eq!(observed[4], Duration::from_millis(400));
        }
        sup.shutdown();
    }

    #[test]
    fn rate_limit_transitions_to_failing() {
        // 3-strike rate limit so we hit Failing quickly.
        let cfg = SupervisorConfig {
            rate_limit_max:    3,
            rate_limit_window: Duration::from_secs(60),
            ..test_config()
        };
        let sup = DaemonSupervisor::new(crash_immediately_spawner(7), cfg);
        sup.start();

        let saw_failing = tick_until(&sup, Duration::from_secs(3), |s| matches!(s, DaemonState::Failing { .. }));
        assert!(saw_failing, "never reached Failing; state={:?}", sup.state());
        if let DaemonState::Failing { last_exit_code } = sup.state() {
            assert_eq!(last_exit_code, Some(7));
        }

        // Once Failing, tick() must NOT respawn anymore even if the
        // backoff timer would otherwise have fired.
        sup.tick();
        sleep(Duration::from_millis(100));
        sup.tick();
        assert!(matches!(sup.state(), DaemonState::Failing { .. }));
        sup.shutdown();
    }

    #[test]
    fn shutdown_returns_to_not_started_and_blocks_restart() {
        let sup = DaemonSupervisor::new(sleep_then_exit_spawner(2000, 0), test_config());
        sup.start();
        assert!(matches!(sup.state(), DaemonState::Running { .. }));
        sup.shutdown();
        assert_eq!(sup.state(), DaemonState::NotStarted);
        // tick() on a shut-down supervisor is a no-op.
        sup.tick();
        assert_eq!(sup.state(), DaemonState::NotStarted);
    }

    #[test]
    fn initial_spawn_failure_schedules_restart() {
        // Spawner that always fails. The first failure happens inside
        // start() and is treated as a crash with no exit code.
        let calls = Arc::new(AtomicU32::new(0));
        let calls_for_spawner = calls.clone();
        let spawner: Spawner = Arc::new(move || -> io::Result<Child> {
            calls_for_spawner.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::new(io::ErrorKind::NotFound, "fake binary missing"))
        });
        let sup = DaemonSupervisor::new(spawner, SupervisorConfig {
            rate_limit_max: 3,
            ..test_config()
        });
        let started = sup.start();
        assert!(!started);
        assert!(matches!(sup.state(), DaemonState::Restarting { .. }));

        // After the rate limit, supervisor must give up.
        let saw_failing = tick_until(&sup, Duration::from_secs(3), |s| matches!(s, DaemonState::Failing { .. }));
        assert!(saw_failing, "never reached Failing; state={:?}", sup.state());
        assert!(calls.load(Ordering::SeqCst) >= 3);
    }

    #[test]
    fn label_format_matches_spec() {
        let sup = DaemonSupervisor::new(sleep_then_exit_spawner(2000, 0), test_config());
        assert_eq!(sup.status_label(), "daemon: not started");
        sup.start();
        assert!(sup.status_label().starts_with("daemon: running"));
        sup.shutdown();
    }
}
