//! MegaClicker MVP startup, wiring, console UI, and shutdown.

#[cfg(not(windows))]
compile_error!("MegaClicker MVP supports Windows 10/11 x64 only.");

mod cli;
mod config;
mod engine;
mod hotkey;
mod input;
mod single_instance;

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;

use crate::cli::Cli;
use crate::config::{Button, ClickType, Config};
use crate::engine::{ClickSink, Clicker, SessionReport, StartOutcome, StopReason};
use crate::hotkey::{HotkeyAction, HotkeyError, Hotkeys};
use crate::single_instance::InstanceError;

const EXIT_HOTKEY_CONFLICT: u8 = 2;
const EXIT_ALREADY_RUNNING: u8 = 3;

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const STATUS_INTERVAL: Duration = Duration::from_secs(5);

/// The live input sink. The engine owns it; all Win32 calls remain in
/// `input.rs`.
struct SystemSink;

impl ClickSink for SystemSink {
    fn click(&self, button: Button, click_type: ClickType) -> Result<(), String> {
        input::send_click(button, click_type).map_err(|error| error.to_string())
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("[error] {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<ExitCode> {
    let cli = Cli::parse();

    println!(
        "MegaClicker {} - lightweight auto clicker for Windows",
        env!("CARGO_PKG_VERSION")
    );

    if let Err(error) = input::install_ctrl_c_handler() {
        eprintln!("[warn] Ctrl+C clean shutdown is unavailable: {error}. Use F8 to quit.");
    }

    let _instance = match single_instance::InstanceGuard::acquire() {
        Ok(guard) => guard,
        Err(error @ InstanceError::AlreadyRunning) => {
            eprintln!("[error] {error}");
            return Ok(ExitCode::from(EXIT_ALREADY_RUNNING));
        }
        Err(error) => return Err(error.into()),
    };

    let config_path = match cli.config.clone() {
        Some(path) => path,
        None => config::default_path().context("cannot determine the default config path")?,
    };

    let loaded = if cli.config.is_some() {
        let config = config::load(&config_path)
            .with_context(|| format!("config file: {}", config_path.display()))?;
        config::Loaded {
            config,
            created: false,
        }
    } else {
        config::load_or_create(&config_path)
            .with_context(|| format!("config file: {}", config_path.display()))?
    };

    print_startup(&config_path, &loaded, cli.dry_run);

    let hotkeys = match Hotkeys::register() {
        Ok(hotkeys) => hotkeys,
        Err(error @ HotkeyError::Conflict { .. }) => {
            eprintln!("[error] {error}");
            return Ok(ExitCode::from(EXIT_HOTKEY_CONFLICT));
        }
        Err(error) => return Err(error.into()),
    };

    let sink: Box<dyn ClickSink> = if cli.dry_run {
        Box::new(engine::DryRunSink)
    } else {
        Box::new(SystemSink)
    };
    let mut clicker = Clicker::spawn(loaded.config, sink)?;

    println!("Ready. Press F6 to start or stop, F8 to quit.");

    event_loop(&hotkeys, &clicker, &loaded.config);

    println!("[exit] shutting down...");
    clicker.shutdown();
    let final_count = clicker.count();
    drop(hotkeys);
    println!("[exit] stopped. Final click count: {final_count}.");

    Ok(ExitCode::SUCCESS)
}

fn event_loop(hotkeys: &Hotkeys, clicker: &Clicker, config: &Config) {
    let mut active_session: Option<u64> = None;
    let mut running_since: Option<Instant> = None;
    let mut last_toggle: Option<Instant> = None;
    let mut last_status = Instant::now();

    loop {
        if input::ctrl_c_pressed() {
            println!("[quit] Ctrl+C received.");
            break;
        }

        if let Some(action) = hotkeys.wait_for_event(EVENT_POLL_INTERVAL) {
            match action {
                HotkeyAction::Toggle => {
                    let now = Instant::now();
                    if hotkey::is_debounced(now, last_toggle) {
                        continue;
                    }
                    last_toggle = Some(now);

                    if clicker.is_running() {
                        if clicker.stop() {
                            println!("[stop] F6 received; stopping...");
                        }
                    } else {
                        match clicker.start() {
                            StartOutcome::Started { session } => {
                                active_session = Some(session);
                                running_since = Some(Instant::now());
                                last_status = Instant::now();
                                print_session_start(config);
                            }
                            StartOutcome::AlreadyRunning => {
                                println!(
                                    "[toggle] ignored: the previous session is still stopping"
                                );
                            }
                        }
                    }
                }
                HotkeyAction::Quit => {
                    println!("[quit] F8 received.");
                    break;
                }
            }
        }

        while let Some(report) = clicker.try_recv_report() {
            print_report(&report);
            if active_session == Some(report.session) {
                active_session = None;
                running_since = None;
            }
        }

        let status_due = last_status.elapsed() >= STATUS_INTERVAL;
        if status_due {
            let status = status_message(clicker, running_since);
            if let Some(message) = status {
                println!("{message}");
                last_status = Instant::now();
            }
        }
    }
}

/// Build a status line when a session is running and the reporting interval has
/// elapsed. Keeping this pure makes the "only while running" rule explicit.
fn status_message(clicker: &Clicker, running_since: Option<Instant>) -> Option<String> {
    if !clicker.is_running() {
        return None;
    }

    let started = running_since?;
    let count = clicker.count();
    let elapsed = started.elapsed();

    Some(format!(
        "[status] running: {count} clicks in {} ({:.1} CPS)",
        format_duration(elapsed),
        clicks_per_second(count, elapsed)
    ))
}

fn print_startup(path: &Path, loaded: &config::Loaded, dry_run: bool) {
    let state = if loaded.created {
        "created with defaults"
    } else {
        "loaded"
    };

    println!("Config: {} ({state})", path.display());
    println!(
        "  interval_ms         = {}",
        loaded.config.interval.as_millis()
    );
    println!("  button              = {}", loaded.config.button);
    println!("  click_type          = {}", loaded.config.click_type);
    if loaded.config.click_limit == 0 {
        println!("  click_limit         = 0 (unlimited)");
    } else {
        println!("  click_limit         = {}", loaded.config.click_limit);
    }
    println!(
        "  start_delay_seconds = {}",
        loaded.config.start_delay.as_secs()
    );
    println!(
        "Mode: {}",
        if dry_run {
            "dry-run (no mouse events are sent)"
        } else {
            "live (clicks are sent at the current cursor position)"
        }
    );
    println!("Hotkeys: F6 = start/stop | F8 = quit | Ctrl+C = quit");
}

fn print_session_start(config: &Config) {
    let limit = if config.click_limit == 0 {
        "unlimited".to_owned()
    } else {
        config.click_limit.to_string()
    };

    println!(
        "[start] interval={} ms | button={} | type={} | limit={limit} | start delay={} s",
        config.interval.as_millis(),
        config.button,
        config.click_type,
        config.start_delay.as_secs()
    );
}

fn print_report(report: &SessionReport) {
    let elapsed = format_duration(report.elapsed);
    let rate = report.clicks_per_second();

    match report.reason {
        StopReason::User => {
            if let Some(latency) = report.stop_latency {
                println!(
                    "[stop] user requested after {} clicks in {elapsed} ({rate:.1} CPS); \
                     stop latency {:.2} ms",
                    report.count,
                    latency.as_secs_f64() * 1000.0
                );
            } else {
                println!(
                    "[stop] user requested after {} clicks in {elapsed} ({rate:.1} CPS)",
                    report.count
                );
            }
        }
        StopReason::LimitReached => {
            println!(
                "[stop] click limit reached after {} clicks in {elapsed} ({rate:.1} CPS)",
                report.count
            );
        }
        StopReason::InputFailed => {
            let error = report.error.as_deref().unwrap_or("unknown input error");
            println!(
                "[error] input failed after {} clicks: {error}",
                report.count
            );
        }
    }
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() >= 1 {
        format!("{:.2} s", duration.as_secs_f64())
    } else {
        format!("{:.1} ms", duration.as_secs_f64() * 1000.0)
    }
}

fn clicks_per_second(count: u64, elapsed: Duration) -> f64 {
    let seconds = elapsed.as_secs_f64();
    if seconds > 0.0 {
        count as f64 / seconds
    } else {
        0.0
    }
}
