//! MegaClicker startup, wiring, console UI, and shutdown.

#[cfg(not(windows))]
compile_error!("MegaClicker supports Windows 10/11 x64 only.");

mod cli;
mod config;
mod engine;
mod hotkey;
mod input;
mod profile;
mod single_instance;
mod tray;

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
use crate::tray::{Tray, TrayEvent};

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
        eprintln!(
            "[warn] Ctrl+C clean shutdown is unavailable: {error}. Use the configured quit hotkey."
        );
    }

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

    if cli.list_profiles {
        print_profiles(&config_path);
        return Ok(ExitCode::SUCCESS);
    }

    let _instance = match single_instance::InstanceGuard::acquire() {
        Ok(guard) => guard,
        Err(error @ InstanceError::AlreadyRunning) => {
            eprintln!("[error] {error}");
            return Ok(ExitCode::from(EXIT_ALREADY_RUNNING));
        }
        Err(error) => return Err(error.into()),
    };

    let (profile_name, active_config) = match &cli.profile {
        Some(name) => {
            let profile = profile::load_or_create(&config_path, name).with_context(|| {
                format!(
                    "profile `{name}` in {}",
                    profile::directory(&config_path).display()
                )
            })?;
            let active = loaded.config.with_click_settings(&profile.settings);
            (profile.name, active)
        }
        None => (profile::DEFAULT_PROFILE.to_owned(), loaded.config),
    };

    print_startup(
        &config_path,
        &loaded,
        &active_config,
        &profile_name,
        cli.dry_run,
    );

    let hotkeys = match Hotkeys::register(
        active_config.toggle_hotkey,
        active_config.quit_hotkey,
        active_config.profile_hotkey,
    ) {
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
    let mut clicker = Clicker::spawn(active_config, sink)?;

    let tray = match Tray::install("MegaClicker - press the toggle hotkey to click") {
        Ok(tray) => Some(tray),
        Err(error) => {
            eprintln!("[warn] tray icon unavailable: {error}");
            None
        }
    };

    println!(
        "Ready. Press {} to start or stop, {} to switch profile, {} to quit.",
        active_config.toggle_hotkey, active_config.profile_hotkey, active_config.quit_hotkey
    );

    event_loop(
        &hotkeys,
        &clicker,
        &loaded.config,
        &config_path,
        &profile_name,
        active_config,
        tray.as_ref(),
    );

    println!("[exit] shutting down...");
    clicker.shutdown();
    let final_count = clicker.count();
    drop(hotkeys);
    println!("[exit] stopped. Final click count: {final_count}.");

    Ok(ExitCode::SUCCESS)
}

fn event_loop(
    hotkeys: &Hotkeys,
    clicker: &Clicker,
    base_config: &Config,
    config_path: &Path,
    initial_profile: &str,
    initial_config: Config,
    tray: Option<&Tray>,
) {
    let mut active_profile = initial_profile.to_owned();
    let mut active_config = initial_config;
    let mut active_session: Option<u64> = None;
    let mut running_since: Option<Instant> = None;
    let mut last_toggle: Option<Instant> = None;
    let mut last_status = Instant::now();

    'outer: loop {
        if input::ctrl_c_pressed() {
            println!("[quit] Ctrl+C received.");
            break;
        }

        if let Some(tray) = tray {
            tray.pump();
            while let Some(event) = tray.try_recv() {
                match event {
                    TrayEvent::Toggle => toggle_clicking(
                        clicker,
                        &active_profile,
                        &active_config,
                        "the tray menu",
                        &mut active_session,
                        &mut running_since,
                        &mut last_status,
                    ),
                    TrayEvent::NextProfile => switch_profile(
                        clicker,
                        base_config,
                        config_path,
                        &mut active_profile,
                        &mut active_config,
                    ),
                    TrayEvent::OpenConfig => open_with_shell(config_path),
                    TrayEvent::OpenProfiles => open_profiles_folder(config_path),
                    TrayEvent::Quit => {
                        println!("[quit] tray menu.");
                        break 'outer;
                    }
                }
            }
        }

        if let Some(action) = hotkeys.wait_for_event(EVENT_POLL_INTERVAL) {
            match action {
                HotkeyAction::Toggle => {
                    let now = Instant::now();
                    if hotkey::is_debounced(now, last_toggle) {
                        continue;
                    }
                    last_toggle = Some(now);

                    let source = active_config.toggle_hotkey.as_str();
                    toggle_clicking(
                        clicker,
                        &active_profile,
                        &active_config,
                        source,
                        &mut active_session,
                        &mut running_since,
                        &mut last_status,
                    );
                }
                HotkeyAction::NextProfile => {
                    switch_profile(
                        clicker,
                        base_config,
                        config_path,
                        &mut active_profile,
                        &mut active_config,
                    );
                }
                HotkeyAction::Quit => {
                    println!("[quit] {} received.", active_config.quit_hotkey);
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

/// Start or stop clicking from either the hotkey or the tray menu.
fn toggle_clicking(
    clicker: &Clicker,
    profile_name: &str,
    config: &Config,
    source: &str,
    active_session: &mut Option<u64>,
    running_since: &mut Option<Instant>,
    last_status: &mut Instant,
) {
    if clicker.is_running() {
        if clicker.stop() {
            println!("[stop] stop requested from {source}; stopping...");
        }
        return;
    }

    match clicker.start() {
        StartOutcome::Started { session } => {
            *active_session = Some(session);
            *running_since = Some(Instant::now());
            *last_status = Instant::now();
            print_session_start(profile_name, config);
        }
        StartOutcome::AlreadyRunning => {
            println!("[toggle] ignored: the previous session is still stopping");
        }
    }
}

fn open_with_shell(path: &Path) {
    if let Err(error) = tray::open_path(path) {
        println!("[warn] could not open {}: {error}", path.display());
    }
}

fn open_profiles_folder(config_path: &Path) {
    let directory = profile::directory(config_path);

    match std::fs::create_dir_all(&directory) {
        Ok(()) => open_with_shell(&directory),
        Err(error) => println!("[warn] could not create {}: {error}", directory.display()),
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

fn print_profiles(config_path: &Path) {
    println!("Profiles in {}:", profile::directory(config_path).display());
    println!("  default");
    println!("    {}", config_path.display());

    match profile::list(config_path) {
        Ok(names) if names.is_empty() => println!("  (no named profiles yet)"),
        Ok(names) => {
            for name in names {
                if let Ok(path) = profile::path_for(config_path, &name) {
                    println!("  {name}");
                    println!("    {}", path.display());
                }
            }
        }
        Err(error) => println!("[error] {error}"),
    }

    println!(
        "Start with --profile <NAME> to use a profile; a missing file is created with defaults."
    );
}

fn print_startup(
    path: &Path,
    loaded: &config::Loaded,
    active: &Config,
    profile_name: &str,
    dry_run: bool,
) {
    let state = if loaded.created {
        "created with defaults"
    } else {
        "loaded"
    };

    println!("Config: {} ({state})", path.display());
    if profile_name.eq_ignore_ascii_case(profile::DEFAULT_PROFILE) {
        println!("Profile: default (click settings from config.toml)");
    } else if let Ok(profile_path) = profile::path_for(path, profile_name) {
        println!("Profile: {profile_name} ({})", profile_path.display());
    } else {
        println!("Profile: {profile_name}");
    }
    println!("  interval_ms         = {}", active.interval.as_millis());
    println!("  button              = {}", active.button);
    println!("  click_type          = {}", active.click_type);
    if active.click_limit == 0 {
        println!("  click_limit         = 0 (unlimited)");
    } else {
        println!("  click_limit         = {}", active.click_limit);
    }
    println!("  start_delay_seconds = {}", active.start_delay.as_secs());
    println!("  toggle_hotkey       = {}", active.toggle_hotkey);
    println!("  quit_hotkey         = {}", active.quit_hotkey);
    println!("  profile_hotkey      = {}", active.profile_hotkey);
    println!(
        "Mode: {}",
        if dry_run {
            "dry-run (no mouse events are sent)"
        } else {
            "live (clicks are sent at the current cursor position)"
        }
    );
    println!(
        "Hotkeys: {} = start/stop | {} = next profile | {} = quit | Ctrl+C = quit",
        active.toggle_hotkey, active.profile_hotkey, active.quit_hotkey
    );
}

/// Advance to the next profile and apply its click settings to the next
/// session. Hotkeys and the running session are deliberately untouched.
fn switch_profile(
    clicker: &Clicker,
    base_config: &Config,
    config_path: &Path,
    active_profile: &mut String,
    active_config: &mut Config,
) {
    let outcome = match profile::cycle(config_path, active_profile) {
        Ok(outcome) => outcome,
        Err(error) => {
            println!("[error] cannot switch profile: {error}");
            return;
        }
    };

    for warning in outcome.skipped {
        println!("[warn] skipped profile: {warning}");
    }

    let name = outcome.selection.name().to_owned();
    let config = outcome.selection.apply(base_config);

    if name.eq_ignore_ascii_case(active_profile) {
        println!("[profile] no other usable profile is available; staying on `{name}`.");
        return;
    }

    *active_profile = name;
    *active_config = config;
    clicker.set_config(config);

    println!(
        "[profile] active: {active_profile} | {}",
        settings_summary(&config)
    );
    if clicker.is_running() {
        println!(
            "[profile] the running session keeps its settings; stop and start to use this profile."
        );
    } else {
        println!("[profile] the new settings apply to the next session.");
    }
}

fn settings_summary(config: &Config) -> String {
    let limit = if config.click_limit == 0 {
        "unlimited".to_owned()
    } else {
        config.click_limit.to_string()
    };

    format!(
        "interval={} ms | button={} | type={} | limit={limit} | start delay={} s",
        config.interval.as_millis(),
        config.button,
        config.click_type,
        config.start_delay.as_secs()
    )
}

fn print_session_start(profile_name: &str, config: &Config) {
    println!(
        "[start] profile={profile_name} | {}",
        settings_summary(config)
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
