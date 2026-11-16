//! The click engine: one dedicated worker thread, absolute scheduling, and a
//! fast stop path.
//!
//! The worker sleeps on a condition variable while idle. When started, it
//! schedules click deadlines from a fixed origin so timing does not drift.
//! Stop requests set an atomic flag and wake any current wait within a few
//! milliseconds; the main thread never joins a busy loop.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::config::{Button, ClickType, Config};
use crate::random::Rng;

/// Spin only for the final part of a wait. This keeps CPU use low at normal
/// click rates while still landing on the deadline accurately.
const SPIN_WINDOW: Duration = Duration::from_micros(500);

/// Largest native sleep slice. Sleeping in slices lets a stop request abort a
/// long interval within a few milliseconds instead of waiting for the full
/// interval to elapse.
const MAX_SLEEP_SLICE: Duration = Duration::from_millis(5);

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("could not spawn the clicking thread: {0}")]
    Spawn(#[source] std::io::Error),
}

/// One click request. The cursor jitter is resolved by the sink because only it
/// knows where the pointer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClickRequest {
    pub button: Button,
    pub click_type: ClickType,
    /// Maximum horizontal and vertical offset from the session anchor, in
    /// pixels. 0 keeps the cursor where it is.
    pub cursor_jitter_px: u32,
}

/// Receives one call per click action. Implementations may inject input or do
/// nothing (dry-run).
pub trait ClickSink: Send + 'static {
    /// Called once before the first click of a session, before the start-delay
    /// wait, so an implementation can forget any previous pointer anchor.
    fn begin_session(&self) {}

    fn click(&self, request: ClickRequest) -> Result<(), String>;
}

/// A sink that performs no input, used by `--dry-run` and engine tests.
pub struct DryRunSink;

impl ClickSink for DryRunSink {
    fn click(&self, _request: ClickRequest) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    Started {
        session: u64,
    },
    /// A session is already running, or the previous one is still winding down.
    AlreadyRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    User,
    LimitReached,
    InputFailed,
}

/// Sent exactly once when a session ends. `count` is the final click count of
/// that session.
#[derive(Debug)]
pub struct SessionReport {
    pub session: u64,
    pub count: u64,
    pub reason: StopReason,
    /// Measured from the start command to the end of the session.
    pub elapsed: Duration,
    /// Present when the session was stopped by the user or by Ctrl+C/F8.
    pub stop_latency: Option<Duration>,
    pub error: Option<String>,
}

impl SessionReport {
    pub fn clicks_per_second(&self) -> f64 {
        let seconds = self.elapsed.as_secs_f64();
        if seconds > 0.0 {
            self.count as f64 / seconds
        } else {
            0.0
        }
    }
}

struct WorkerState {
    busy: bool,
    shutdown: bool,
}

struct Shared {
    state: Mutex<WorkerState>,
    /// Read once at the start of every session, so a profile switch never
    /// changes the settings of a session that is already running.
    config: Mutex<Config>,
    wake: Condvar,
    stop: AtomicBool,
    running: AtomicBool,
    count: AtomicU64,
    session: AtomicU64,
    stop_requested_at: Mutex<Option<Instant>>,
    events: mpsc::Sender<SessionReport>,
}

/// Handle to the clicking thread. Dropping it shuts the thread down.
pub struct Clicker {
    shared: Arc<Shared>,
    events: mpsc::Receiver<SessionReport>,
    handle: Option<JoinHandle<()>>,
}

impl Clicker {
    pub fn spawn(config: Config, sink: Box<dyn ClickSink>) -> Result<Self, EngineError> {
        let (events, receiver) = mpsc::channel();
        let shared = Arc::new(Shared {
            state: Mutex::new(WorkerState {
                busy: false,
                shutdown: false,
            }),
            config: Mutex::new(config),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            running: AtomicBool::new(false),
            count: AtomicU64::new(0),
            session: AtomicU64::new(0),
            stop_requested_at: Mutex::new(None),
            events,
        });

        let worker_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("megaclicker-clicker".to_owned())
            .spawn(move || worker_loop(worker_shared, sink))
            .map_err(EngineError::Spawn)?;

        Ok(Self {
            shared,
            events: receiver,
            handle: Some(handle),
        })
    }

    /// Start a new session. Starting while a session is active (or still
    /// winding down) is a no-op.
    pub fn start(&self) -> StartOutcome {
        let mut state = lock(&self.shared.state);
        if state.busy || state.shutdown {
            return StartOutcome::AlreadyRunning;
        }

        let session = self.shared.session.fetch_add(1, Ordering::SeqCst) + 1;
        self.shared.stop.store(false, Ordering::SeqCst);
        self.shared.count.store(0, Ordering::SeqCst);
        *lock(&self.shared.stop_requested_at) = None;
        self.shared.running.store(true, Ordering::SeqCst);
        state.busy = true;
        self.shared.wake.notify_all();

        StartOutcome::Started { session }
    }

    /// Request a stop. Returns false when no session is running.
    pub fn stop(&self) -> bool {
        let _state = lock(&self.shared.state);
        if !self.shared.running.load(Ordering::SeqCst) {
            return false;
        }

        self.shared.stop.store(true, Ordering::SeqCst);
        *lock(&self.shared.stop_requested_at) = Some(Instant::now());
        self.shared.running.store(false, Ordering::SeqCst);
        self.shared.wake.notify_all();
        true
    }

    /// Replace the settings used by the next session. A running session keeps
    /// the configuration it started with.
    pub fn set_config(&self, config: Config) {
        *lock(&self.shared.config) = config;
    }

    pub fn is_running(&self) -> bool {
        self.shared.running.load(Ordering::SeqCst)
    }

    /// Clicks completed in the current or most recent session.
    pub fn count(&self) -> u64 {
        self.shared.count.load(Ordering::SeqCst)
    }

    pub fn try_recv_report(&self) -> Option<SessionReport> {
        self.events.try_recv().ok()
    }

    /// Stop clicking, wake the worker, and join it. Idempotent.
    pub fn shutdown(&mut self) {
        {
            let mut state = lock(&self.shared.state);
            state.shutdown = true;
            self.shared.stop.store(true, Ordering::SeqCst);
            self.shared.running.store(false, Ordering::SeqCst);
            self.shared.wake.notify_all();
        }

        if let Some(handle) = self.handle.take() {
            // A panic in the worker is a bug. Shutdown must still complete, so
            // the join result is deliberately ignored here.
            let _ = handle.join();
        }
    }
}

impl Drop for Clicker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned mutex means a thread panicked while holding it. The engine's
    // critical sections are tiny and contain no user code, so recovering the
    // guard is safer than aborting shutdown.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn worker_loop(shared: Arc<Shared>, sink: Box<dyn ClickSink>) {
    loop {
        let (session, config) = {
            let mut state = lock(&shared.state);
            while !state.busy && !state.shutdown {
                state = shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }

            if state.shutdown && !state.busy {
                return;
            }

            (shared.session.load(Ordering::SeqCst), *lock(&shared.config))
        };

        let origin = Instant::now();
        let outcome = run_session(&shared, &config, sink.as_ref(), origin, session);
        let count = shared.count.load(Ordering::SeqCst);

        let shutting_down = {
            let mut state = lock(&shared.state);
            state.busy = false;
            shared.running.store(false, Ordering::SeqCst);
            shared.wake.notify_all();
            state.shutdown
        };

        if !shutting_down {
            let report = SessionReport {
                session,
                count,
                reason: outcome.reason,
                elapsed: origin.elapsed(),
                stop_latency: outcome.stop_latency,
                error: outcome.error,
            };
            // The main thread may already be gone during process teardown.
            let _ = shared.events.send(report);
        }

        if shutting_down {
            return;
        }
    }
}

struct Outcome {
    reason: StopReason,
    stop_latency: Option<Duration>,
    error: Option<String>,
}

fn run_session(
    shared: &Shared,
    config: &Config,
    sink: &dyn ClickSink,
    origin: Instant,
    session: u64,
) -> Outcome {
    sink.begin_session();

    let mut schedule = Schedule::new(
        origin,
        config.start_delay,
        config.interval,
        config.interval_jitter_percent,
        crate::random::seed().wrapping_add(session),
    );

    loop {
        if !wait_until(schedule.deadline(), &shared.stop) {
            return stopped_by_user(shared);
        }

        // Close the tiny race between the wait returning and the send below.
        if shared.stop.load(Ordering::Acquire) {
            return stopped_by_user(shared);
        }

        let request = ClickRequest {
            button: config.button,
            click_type: config.click_type,
            cursor_jitter_px: u32::try_from(config.cursor_jitter_px).unwrap_or(u32::MAX),
        };
        if let Err(error) = sink.click(request) {
            return Outcome {
                reason: StopReason::InputFailed,
                stop_latency: None,
                error: Some(error),
            };
        }

        let count = shared.count.fetch_add(1, Ordering::SeqCst) + 1;
        if limit_reached(count, config.click_limit) {
            return Outcome {
                reason: StopReason::LimitReached,
                stop_latency: None,
                error: None,
            };
        }

        // Advance from the previous deadline, not from "now", so a slow click
        // does not accumulate drift over the session.
        schedule.advance();
    }
}

/// Absolute deadline generator. `interval` is measured from deadline to
/// deadline, which is the start-to-start interval required by the spec.
/// Optional jitter picks a fresh interval for every step.
#[derive(Debug, Clone, Copy)]
struct Schedule {
    next: Instant,
    interval: Duration,
    jitter_percent: u64,
    rng: Rng,
}

impl Schedule {
    fn new(
        origin: Instant,
        start_delay: Duration,
        interval: Duration,
        jitter_percent: u64,
        seed: u64,
    ) -> Self {
        Self {
            next: origin.checked_add(start_delay).unwrap_or(origin),
            interval,
            jitter_percent,
            rng: Rng::seeded(seed),
        }
    }

    fn deadline(&self) -> Instant {
        self.next
    }

    fn advance(&mut self) {
        let interval = jittered_interval(self.interval, self.jitter_percent, &mut self.rng);
        self.next = self.next.checked_add(interval).unwrap_or_else(Instant::now);
    }
}

/// Pick the next interval. Jitter is a percentage of the configured interval
/// and is clamped so the effective interval never drops below one millisecond.
fn jittered_interval(interval: Duration, percent: u64, rng: &mut Rng) -> Duration {
    if percent == 0 {
        return interval;
    }

    let percent = percent.min(100);
    let base = interval.as_millis() as u64;
    let low = (base * (100 - percent) / 100).max(1);
    let high = (base * (100 + percent) / 100).max(low);

    Duration::from_millis(low + rng.below(high - low + 1))
}

fn stopped_by_user(shared: &Shared) -> Outcome {
    let requested_at = *lock(&shared.stop_requested_at);
    Outcome {
        reason: StopReason::User,
        stop_latency: requested_at.map(|requested| requested.elapsed()),
        error: None,
    }
}

/// Wait until `deadline`, returning false as soon as `stop` becomes true.
fn wait_until(deadline: Instant, stop: &AtomicBool) -> bool {
    loop {
        if stop.load(Ordering::Acquire) {
            return false;
        }

        let now = Instant::now();
        if now >= deadline {
            return true;
        }

        let remaining = deadline - now;
        if remaining > MAX_SLEEP_SLICE {
            // Bulk wait, no spinning. A stop request is noticed at most one
            // slice later.
            spin_sleep::native_sleep(MAX_SLEEP_SLICE);
        } else if remaining > SPIN_WINDOW {
            // Leave only the spin window for the accurate finish.
            spin_sleep::native_sleep(remaining - SPIN_WINDOW);
        } else {
            while Instant::now() < deadline {
                if stop.load(Ordering::Acquire) {
                    return false;
                }
                std::hint::spin_loop();
            }
            return true;
        }
    }
}

/// Pure click-limit rule: 0 means unlimited.
pub fn limit_reached(count: u64, limit: u64) -> bool {
    limit != 0 && count >= limit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HotkeyKey;
    use std::sync::Mutex as StdMutex;

    /// Records every click so tests can prove which settings were used.
    struct RecordingSink(Arc<StdMutex<Vec<ClickRequest>>>);

    impl ClickSink for RecordingSink {
        fn click(&self, request: ClickRequest) -> Result<(), String> {
            self.0.lock().expect("recording mutex").push(request);
            Ok(())
        }
    }

    fn test_config(interval_ms: u64, click_limit: u64) -> Config {
        Config {
            interval: Duration::from_millis(interval_ms),
            interval_jitter_percent: 0,
            button: Button::Left,
            click_type: ClickType::Single,
            cursor_jitter_px: 0,
            toggle_hotkey: HotkeyKey::F6,
            quit_hotkey: HotkeyKey::F8,
            profile_hotkey: HotkeyKey::F7,
            click_limit,
            start_delay: Duration::ZERO,
        }
    }

    #[test]
    fn click_limit_rule_exact() {
        assert!(!limit_reached(0, 0));
        assert!(!limit_reached(1, 0));
        assert!(!limit_reached(999_999_999, 0));

        assert!(!limit_reached(9, 10));
        assert!(limit_reached(10, 10));
        assert!(limit_reached(11, 10));
    }

    #[test]
    fn schedule_advances_by_exact_intervals() {
        let origin = Instant::now();
        let mut schedule = Schedule::new(
            origin,
            Duration::from_millis(250),
            Duration::from_millis(100),
            0,
            1,
        );

        assert_eq!(schedule.deadline(), origin + Duration::from_millis(250));
        schedule.advance();
        assert_eq!(schedule.deadline(), origin + Duration::from_millis(350));
        schedule.advance();
        assert_eq!(schedule.deadline(), origin + Duration::from_millis(450));
        schedule.advance();
        assert_eq!(schedule.deadline(), origin + Duration::from_millis(550));
    }

    #[test]
    fn schedule_without_delay_starts_immediately() {
        let origin = Instant::now();
        let mut schedule = Schedule::new(origin, Duration::ZERO, Duration::from_millis(10), 0, 1);

        assert_eq!(schedule.deadline(), origin);
        schedule.advance();
        assert_eq!(schedule.deadline(), origin + Duration::from_millis(10));
    }

    #[test]
    fn limit_stops_session_and_reports_count() {
        let mut clicker =
            Clicker::spawn(test_config(10, 3), Box::new(DryRunSink)).expect("spawn clicker");
        let started = Instant::now();

        assert!(matches!(clicker.start(), StartOutcome::Started { .. }));

        let report = clicker
            .events
            .recv_timeout(Duration::from_secs(2))
            .expect("session report");

        assert_eq!(report.reason, StopReason::LimitReached);
        assert_eq!(report.count, 3);
        assert!(report.elapsed >= Duration::from_millis(20));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!clicker.is_running());

        clicker.shutdown();
    }

    #[test]
    fn user_stop_is_fast_and_reported() {
        let mut clicker =
            Clicker::spawn(test_config(5, 0), Box::new(DryRunSink)).expect("spawn clicker");
        let start_time = Instant::now();

        assert!(matches!(clicker.start(), StartOutcome::Started { .. }));
        thread::sleep(Duration::from_millis(30));

        assert!(clicker.stop());
        let report = clicker
            .events
            .recv_timeout(Duration::from_secs(2))
            .expect("session report");

        assert_eq!(report.reason, StopReason::User);
        assert!(report.count >= 1);
        assert!(report.stop_latency.is_some());
        assert!(start_time.elapsed() < Duration::from_secs(2));

        clicker.shutdown();
    }

    #[test]
    fn start_and_stop_no_ops_are_safe() {
        let mut clicker =
            Clicker::spawn(test_config(100, 0), Box::new(DryRunSink)).expect("spawn clicker");

        assert!(!clicker.stop(), "stopping an idle engine must be a no-op");

        assert!(matches!(clicker.start(), StartOutcome::Started { .. }));
        assert_eq!(
            clicker.start(),
            StartOutcome::AlreadyRunning,
            "starting twice must not create a second session"
        );

        assert!(clicker.stop());
        assert!(
            !clicker.stop(),
            "stopping twice must be a no-op once the stop is recorded"
        );

        clicker.shutdown();
    }

    #[test]
    fn new_config_applies_to_the_next_session_only() {
        let recorded = Arc::new(StdMutex::new(Vec::new()));
        let mut clicker = Clicker::spawn(
            test_config(5, 1),
            Box::new(RecordingSink(Arc::clone(&recorded))),
        )
        .expect("spawn clicker");

        assert!(matches!(clicker.start(), StartOutcome::Started { .. }));
        clicker
            .events
            .recv_timeout(Duration::from_secs(2))
            .expect("first report");

        let mut next = test_config(5, 1);
        next.button = Button::Right;
        next.click_type = ClickType::Double;
        clicker.set_config(next);

        assert!(matches!(clicker.start(), StartOutcome::Started { .. }));
        clicker
            .events
            .recv_timeout(Duration::from_secs(2))
            .expect("second report");

        assert_eq!(
            *recorded.lock().expect("recording mutex"),
            vec![
                ClickRequest {
                    button: Button::Left,
                    click_type: ClickType::Single,
                    cursor_jitter_px: 0,
                },
                ClickRequest {
                    button: Button::Right,
                    click_type: ClickType::Double,
                    cursor_jitter_px: 0,
                }
            ]
        );

        clicker.shutdown();
    }

    #[test]
    fn startup_with_start_delay_still_reports_cleanly() {
        let mut config = test_config(10, 1);
        config.start_delay = Duration::from_millis(20);
        let mut clicker = Clicker::spawn(config, Box::new(DryRunSink)).expect("spawn clicker");
        let started = Instant::now();

        assert!(matches!(clicker.start(), StartOutcome::Started { .. }));

        let report = clicker
            .events
            .recv_timeout(Duration::from_secs(2))
            .expect("session report");

        assert_eq!(report.reason, StopReason::LimitReached);
        assert_eq!(report.count, 1);
        assert!(started.elapsed() >= Duration::from_millis(20));

        clicker.shutdown();
    }

    #[test]
    fn cursor_jitter_reaches_the_sink() {
        let recorded = Arc::new(StdMutex::new(Vec::new()));
        let mut config = test_config(5, 1);
        config.cursor_jitter_px = 7;
        let mut clicker = Clicker::spawn(config, Box::new(RecordingSink(Arc::clone(&recorded))))
            .expect("spawn clicker");

        assert!(matches!(clicker.start(), StartOutcome::Started { .. }));
        clicker
            .events
            .recv_timeout(Duration::from_secs(2))
            .expect("session report");

        assert_eq!(
            *recorded.lock().expect("recording mutex"),
            vec![ClickRequest {
                button: Button::Left,
                click_type: ClickType::Single,
                cursor_jitter_px: 7,
            }]
        );

        clicker.shutdown();
    }

    #[test]
    fn zero_jitter_keeps_the_configured_interval() {
        let mut rng = Rng::seeded(11);
        let interval = Duration::from_millis(37);

        for _ in 0..50 {
            assert_eq!(jittered_interval(interval, 0, &mut rng), interval);
        }
    }

    #[test]
    fn jittered_intervals_stay_in_range_and_vary() {
        let mut rng = Rng::seeded(9);
        let mut values: Vec<u64> = (0..500)
            .map(|_| jittered_interval(Duration::from_millis(100), 50, &mut rng).as_millis() as u64)
            .collect();

        assert!(values.iter().all(|value| (50..=150).contains(value)));
        values.sort_unstable();
        values.dedup();
        assert!(
            values.len() > 50,
            "expected many distinct intervals, got {values:?}"
        );
    }

    #[test]
    fn jitter_never_produces_a_zero_interval() {
        let mut rng = Rng::seeded(3);

        for _ in 0..500 {
            let interval = jittered_interval(Duration::from_millis(1), 50, &mut rng);
            assert!(interval >= Duration::from_millis(1));
        }
    }

    #[test]
    fn jitter_range_covers_the_requested_percentage() {
        let mut rng = Rng::seeded(5);
        let mut lowest = u64::MAX;
        let mut highest = 0;

        for _ in 0..2_000 {
            let value =
                jittered_interval(Duration::from_millis(1000), 50, &mut rng).as_millis() as u64;
            lowest = lowest.min(value);
            highest = highest.max(value);
        }

        assert!(lowest >= 500, "lowest was {lowest}");
        assert!(highest <= 1500, "highest was {highest}");
        assert!(lowest < 600, "lowest was {lowest}");
        assert!(highest > 1400, "highest was {highest}");
    }
}
