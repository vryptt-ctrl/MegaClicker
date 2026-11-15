# MegaClicker

A very light but very powerful auto clicker for **Windows 10/11 x64**, written
in Rust. MegaClicker runs in a console with a small notification-area icon, uses
fixed global hotkeys, and keeps all behavior in one small TOML file. It has no
settings window, no network access, and no telemetry.

## What it is

MegaClicker's MVP does one job: click reliably at a configured interval, under
global hotkey control, with measurable timing and clean shutdown. Everything
needed for that is included; everything else is deliberately left out.

- Configurable global function keys start/stop clicking, switch profiles, and quit, even when another window is focused. Defaults are `F6`, `F7`, and `F8`.
- Named profiles store alternate click settings in separate TOML files and can be switched at runtime with `F7`.
- A tray icon mirrors the hotkeys: start/stop, switch profile, open the config or
  profiles folder, and exit.
- `Ctrl+C` also quits cleanly, stopping the click thread.
- Interval, mouse button, single/double click, click limit, and start delay are
  configurable in `%APPDATA%\MegaClicker\config.toml`.
- `--dry-run` follows the real timing and counts clicks without sending input.
- A named mutex prevents two copies from running at once.
- A dedicated click thread with an atomic stop flag and condition-variable wake
  keeps idle CPU at zero and makes stopping effectively instant.

### Not in the MVP

No settings GUI, randomized intervals, cursor movement,
click sequences, recording/playback, scheduler, installer, auto-start,
updater, localization, analytics, crash reporting,
network features, or non-Windows builds.

## Requirements

- Windows 10 or Windows 11, x64.
- [Rust stable](https://rustup.rs/) with the MSVC toolchain
  (`stable-x86_64-pc-windows-msvc`) and the Visual Studio C++ Build Tools.
- No administrator privileges are required.
- No runtime dependencies. The release binary is self-contained.

## Build

From a clean checkout:

```powershell
cargo build --release
```

The binary is written to:

```text
target\release\megaclicker.exe
```

Quality gates used by this project:

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

## Install

There is no installer. Copy `megaclicker.exe` anywhere you like and run it.
To remove MegaClicker, delete the executable and optionally the config folder
`%APPDATA%\MegaClicker`.

## Usage

```powershell
.\megaclicker.exe
.\megaclicker.exe --dry-run
.\megaclicker.exe --config .\my-config.toml
.\megaclicker.exe --profile gaming
.\megaclicker.exe --list-profiles
.\megaclicker.exe --version
```

| Flag | Meaning |
| --- | --- |
| `--config <PATH>` | Use an existing config file instead of the default. A missing file is an error, so typos are not silently replaced. Profiles are stored in a `profiles` folder next to this file. |
| `--profile <NAME>` | Start with the named profile, creating it with defaults if the file does not exist. |
| `--list-profiles` | Print the default profile and every named profile, then exit. |
| `--dry-run` | Count clicks and use the real timing, but do not send mouse input. |
| `--version` | Print the version and exit. |
| `--help` | Print CLI help and exit. |

### Hotkeys

| Key | Action |
| --- | --- |
| Configured `toggle_hotkey` (default `F6`) | Start clicking, or stop the current session. Works globally, even when MegaClicker is not the focused window. |
| Configured `profile_hotkey` (default `F7`) | Switch to the next usable profile. |
| Configured `quit_hotkey` (default `F8`) | Quit MegaClicker. |
| `Ctrl+C` | Quit MegaClicker. |

Toggle presses within 250 ms are ignored (debounce). Starting while already
running is a no-op; stopping while stopped is a no-op. All three hotkeys can be
set to different function keys from F1 through F12 in the configuration.

### Tray menu

MegaClicker adds a notification-area icon while it runs. Right-click it for:

| Item | Action |
| --- | --- |
| Start / stop clicking | Same as the toggle hotkey. |
| Next profile | Same as the profile hotkey. |
| Open config | Opens `config.toml` in the default editor. |
| Open profiles folder | Opens the `profiles` folder, creating it if needed. |
| Exit | Quits MegaClicker cleanly. |

Double-clicking the icon opens the config file. If the shell refuses to add the
icon, MegaClicker prints a warning and continues console-only.

### Startup output

```text
MegaClicker 0.3.0 - lightweight auto clicker for Windows
Config: C:\Users\you\AppData\Roaming\MegaClicker\config.toml (created with defaults)
Profile: default (click settings from config.toml)
  interval_ms         = 100
  button              = left
  click_type          = single
  click_limit         = 0 (unlimited)
  start_delay_seconds = 0
  toggle_hotkey       = F6
  quit_hotkey         = F8
  profile_hotkey      = F7
Mode: live (clicks are sent at the current cursor position)
Hotkeys: F6 = start/stop | F7 = next profile | F8 = quit | Ctrl+C = quit
Ready. Press F6 to start or stop, F7 to switch profile, F8 to quit.
```

While running, MegaClicker prints the start settings, a status line every five
seconds, and a final stop report with the click count, elapsed time, average
CPS, and measured stop latency.

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Normal shutdown from the quit hotkey, the tray menu, or `Ctrl+C`. |
| `1` | Configuration, I/O, or unexpected startup error. |
| `2` | A required global hotkey (toggle, profile, or quit) is already used by another application. |
| `3` | Another MegaClicker instance is already running in this Windows session. |

## Configuration

### Location and lifecycle

The default config file is:

```text
%APPDATA%\MegaClicker\config.toml
```

On first run MegaClicker creates the folder and the file, including comments
that document every setting and its valid range. The committed
[`config.toml`](config.toml) in this repository is the exact file that is
written on first run.

The config is read **once at startup**. Editing it while MegaClicker is running
has no effect; restart MegaClicker to apply changes. Delete the file and run
MegaClicker again to regenerate the defaults.

`--config <PATH>` reads an existing file instead of the default. It does not
auto-create a missing file.

### Settings

| Key | Default | Allowed values | Meaning |
| --- | --- | --- | --- |
| `interval_ms` | `100` | `1`–`1000` | Time between the **start** of consecutive clicks, in milliseconds. |
| `button` | `"left"` | `"left"`, `"right"`, `"middle"` | Mouse button to press. |
| `click_type` | `"single"` | `"single"`, `"double"` | One press/release pair, or two rapid pairs per click action. |
| `click_limit` | `0` | `0`–`1000000000` | Stop after this many click actions. `0` means unlimited. |
| `start_delay_seconds` | `0` | `0`–`5` | Delay before the first click after each start. Applies only to the first click. |
| `toggle_hotkey` | `"F6"` | `"F1"`–`"F12"` | Global key to start/stop clicking. Must differ from the other hotkeys. |
| `quit_hotkey` | `"F8"` | `"F1"`–`"F12"` | Global key to quit. Must differ from the other hotkeys. |
| `profile_hotkey` | `"F7"` | `"F1"`–`"F12"` | Global key to switch to the next profile. Must differ from the other hotkeys. |

String values are matched case-insensitively. Unknown keys are rejected rather
than ignored, so typos cannot silently fall back to a default.

A **double** click action sends two rapid down/up pairs and counts as **one**
click action towards `click_limit`. This keeps the limit independent of the
button event count.

### Example

```toml
# Time between the start of consecutive clicks, in milliseconds.
# Allowed: 1-1000
interval_ms = 100

# Allowed: "left", "right", "middle"
button = "left"

# Allowed: "single", "double"
click_type = "single"

# 0 = unlimited. Allowed: 0-1000000000
click_limit = 0

# Allowed: 0-5
start_delay_seconds = 0

# Global function keys; they must all differ from each other.
# Allowed: "F1" through "F12"
toggle_hotkey = "F6"
quit_hotkey = "F8"
profile_hotkey = "F7"
```

### Profiles

A profile is a named set of click settings stored as its own TOML file:

```text
%APPDATA%\MegaClicker\profiles\gaming.toml
```

Profiles contain only `interval_ms`, `button`, `click_type`, `click_limit`, and
`start_delay_seconds`. Hotkeys are global: they stay registered when profiles
change. A missing profile file is created with defaults the first time it is
selected:

```powershell
.\megaclicker.exe --profile gaming
.\megaclicker.exe --list-profiles
```

Use the configured `profile_hotkey` (default `F7`) to cycle through `default`
and the named profiles in case-insensitive alphabetical order. Switching applies
the new click settings to the **next** session; a session that is already
running keeps the settings it started with. Invalid profile files are reported
and skipped while cycling.

Profile names are limited to 32 characters and may contain ASCII letters,
digits, `-`, and `_` only. `default` is reserved for the click settings in
`config.toml`. The active profile is not persisted: after a restart MegaClicker
uses the base configuration unless `--profile` is passed again.

### Invalid configuration

Invalid values stop startup with an error that names the field and its valid
range, for example:

```text
[error] config file: C:\Users\you\AppData\Roaming\MegaClicker\config.toml: invalid `interval_ms` = 0; expected an integer from 1 to 1000 (milliseconds)
```

A TOML syntax or type error is also reported before any input or hotkey
activity begins.

## Behavior and timing

- **Absolute scheduling.** Click deadlines are derived from a fixed session
  origin, so a slow click does not accumulate drift over time. `interval_ms` is
  measured from the start of one click to the start of the next.
- **One click thread.** A single dedicated thread runs the click loop. Idle
  MegaClicker blocks on a condition variable and uses effectively no CPU.
- **Fast stop.** A stop request sets an atomic flag. Long waits are split into
  at most 5 ms native sleeps, and the final 500 µs of each deadline is a short
  spin, so the worker notices a stop within a few milliseconds. Shutdown joins
  the worker before the process exits.
- **Start delay.** The delay applies once per start, before the first click,
  and is interruptible by the toggle hotkey, the quit hotkey, or `Ctrl+C`.
- **Click limit.** When the configured limit is reached, clicking stops
  automatically and the final count is printed.
- **Dry-run.** `--dry-run` exercises the same thread, scheduling, stop path, and
  click accounting, but substitutes a no-op input sink. It is safe on any
  machine, including remote sessions.

## Performance

Measured on this machine with the release build, using a harness that drives the
real binary through injected `F6`/`F8` hotkeys in `--dry-run` mode. Targets are
from the MVP specification.

| Metric | Target | Measured |
| --- | --- | --- |
| Interval accuracy, 10 CPS | ±10% | 10.3 CPS (30 clicks in 2.90 s) |
| Interval accuracy, 100 CPS | ±10% | 100.1 CPS (1001 clicks in 10.00 s) |
| Stop latency from `F6` | < 50 ms | 2.64 ms |
| Idle CPU | < 0.5% | 0.00% of one core (below timer resolution) |
| Clicking CPU at 50 CPS | < 5% | 0.00% of one core (below timer resolution) |
| Resident memory | < 25 MB | 4.41–4.48 MB |
| Stripped release binary | < 4 MB | 0.688 MB (721,408 bytes) |

CPU figures come from a 30-second `TotalProcessorTime` sample while idle and
while clicking at 50 CPS: both accrued 0 ms of processor time, i.e. below the
15.6 ms Windows timer granularity (less than 0.05% of one core). The reported
CPS is `clicks / session duration`, so very short sessions read slightly high
(for example, 10 clicks at a 50 ms interval finished in 450 ms).

A separate live-input test sent real `SendInput` events to a WinForms window:
10 single-click actions produced 10 `MouseDown` events, and 5 double-click
actions produced 5 `MouseDoubleClick` events, confirming end-to-end delivery.

Timing above 100 CPS is best effort: Windows timer resolution, input queueing,
and the target application's message loop dominate at that point.

## Known limitations and MVP trade-offs

- **Windows only.** The MVP targets Windows 10/11 x64 and uses Win32 directly.
- **Console and tray icon.** The console window must stay open; closing it
  terminates MegaClicker. The tray icon mirrors the main actions but there is no
  settings GUI or hotkey rebinding yet.
- **Function-key hotkeys only.** The toggle, profile, and quit hotkeys can be selected from F1–F12, but modifiers and arbitrary keys are not supported.
- **Current cursor position.** MegaClicker does not move the cursor. Clicks go
  wherever the pointer currently is.
- **Base config requires restart.** The base config is read once at startup.
  Named profiles are re-read whenever they are selected.
- **Active profile is not persisted.** After a restart MegaClicker uses the base
  configuration unless `--profile` is passed again.
- **Elevation / UIPI.** Windows blocks synthetic input sent from a lower
  integrity process to an elevated window. If clicks do not reach an
  administrator window, run both at the same privilege level. MegaClicker
  reports `SendInput` failures when Windows rejects them.
- **Anti-cheat and protected processes.** Some games and security tools block
  synthetic input by design. MegaClicker does not attempt to bypass them.
- **Double-click semantics.** A double action counts as one click action
  towards `click_limit`; it sends two rapid button press/release pairs.
- **Single instance per Windows session.** The named mutex is local to the
  logged-in session, so two different users can each run one copy.
- **Best effort above 100 CPS.** The specification's accuracy target covers
  10–100 CPS. At 1 ms intervals, OS scheduling and input injection become the
  limiting factors.
- **No persistence beyond config.** Counters and session history are not saved.
- **No auto-start or updater.** Start MegaClicker manually, or use Windows Task
  Scheduler yourself if you need that (out of MVP scope).

## Safety and privacy

- MegaClicker makes **no network connections**, has no telemetry, no crash
  reporting, and no auto-update mechanism.
- It writes only the base config file and profile files under
  `%APPDATA%\MegaClicker`.
- Synthetic mouse input is sent only while a session is running.
- `--dry-run` never calls `SendInput`.
- All `unsafe` Rust is confined to two platform modules:
  [`src/input.rs`](src/input.rs), which wraps synthetic input, global hotkeys,
  the message wait, the named mutex, and the console Ctrl+C handler; and
  [`src/tray.rs`](src/tray.rs), which wraps the notification-area icon, its
  hidden window, the popup menu, and `ShellExecuteW`. Each `unsafe` block has a
  `SAFETY:` comment. No other module contains `unsafe`.

## Project layout

```text
src/
  main.rs             startup, wiring, console UI, shutdown
  cli.rs              command-line arguments
  config.rs           load, validate, create defaults, resolve path
  profile.rs          named profile storage, discovery, cycle selection
  hotkey.rs           configurable global hotkeys, conflict errors, debounce
  engine.rs           click thread, state machine, scheduling, tests
  input.rs            Win32 input, hotkey, and message-wait wrappers
  tray.rs             notification-area icon, popup menu, shell launching
  single_instance.rs  named-mutex single-instance guard
config.toml           default config written on first run
```

## Development

```powershell
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

Unit tests cover configuration parsing and every range error, hotkey validation,
profile creation and discovery, profile cycling with broken files, click-limit
accounting, debounce behavior, start/stop no-ops, start delay, applying a new
config to the next session, tray string handling, and a dry-run session that
reaches its limit.

To measure timing safely without clicking anything:

```powershell
.\target\release\megaclicker.exe --dry-run --config .\config.toml
```

Then press `F6` to start and `F6` again to stop. The stop report prints the
click count, elapsed time, average CPS, and measured stop latency. Press `F7`
to cycle profiles and `F8` to exit.

## Roadmap

Done:

- [x] Configurable global hotkeys (`v0.1.1`)
- [x] Multiple profiles and per-profile config switching (`v0.2.0`)
- [x] Tray icon with a native menu (`v0.3.0`)

Planned:

- [ ] Settings GUI, with hotkey rebinding
- [ ] Randomized/human-like intervals and cursor movement (with clear disclaimers)
- [ ] Click sequences, multi-point macros, recording/playback
- [ ] Installer, auto-start, updater
- [ ] Localization
- [ ] macOS/Linux support

## License

MIT. See [LICENSE](LICENSE). The license is not modified by this project.