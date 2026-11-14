//! Fixed global hotkeys for the MVP: F6 toggles clicking, F8 quits.

use std::io;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::input::{self, VirtualKey};

pub const TOGGLE_KEY: &str = "F6";
pub const QUIT_KEY: &str = "F8";

/// Duplicate toggle presses inside this window are ignored.
pub const DEBOUNCE: Duration = Duration::from_millis(250);

const TOGGLE_ID: i32 = 1;
const QUIT_ID: i32 = 2;

// Win32 ERROR_HOTKEY_ALREADY_REGISTERED.
const ERROR_HOTKEY_ALREADY_REGISTERED: i32 = 1409;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    Toggle,
    Quit,
}

#[derive(Debug, Error)]
pub enum HotkeyError {
    #[error(
        "cannot register {key} as a global hotkey because another application is \
         already using it. Close the application, overlay, or driver that owns \
         {key}, then start MegaClicker again."
    )]
    Conflict { key: &'static str },

    #[error("cannot register {key} as a global hotkey: {details}")]
    Register { key: &'static str, details: String },
}

/// Owns the two hotkey registrations and unregisters them on drop.
pub struct Hotkeys;

impl Hotkeys {
    pub fn register() -> Result<Self, HotkeyError> {
        if let Err(error) = input::register_hotkey(TOGGLE_ID, VirtualKey::F6) {
            return Err(map_error(TOGGLE_KEY, error));
        }

        if let Err(error) = input::register_hotkey(QUIT_ID, VirtualKey::F8) {
            // Do not leak the first registration if the second one fails.
            let _ = input::unregister_hotkey(TOGGLE_ID);
            return Err(map_error(QUIT_KEY, error));
        }

        Ok(Self)
    }

    /// Wait up to `timeout` for the next toggle/quit press.
    pub fn wait_for_event(&self, timeout: Duration) -> Option<HotkeyAction> {
        match input::wait_for_hotkey(timeout)? {
            TOGGLE_ID => Some(HotkeyAction::Toggle),
            QUIT_ID => Some(HotkeyAction::Quit),
            _ => None,
        }
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        let _ = input::unregister_hotkey(TOGGLE_ID);
        let _ = input::unregister_hotkey(QUIT_ID);
    }
}

fn map_error(key: &'static str, error: io::Error) -> HotkeyError {
    if error.raw_os_error() == Some(ERROR_HOTKEY_ALREADY_REGISTERED) {
        HotkeyError::Conflict { key }
    } else {
        HotkeyError::Register {
            key,
            details: error.to_string(),
        }
    }
}

/// Whether a toggle arriving at `now` should be ignored because `last` was too
/// recent. Kept pure so the debounce rule is testable.
pub fn is_debounced(now: Instant, last: Option<Instant>) -> bool {
    last.is_some_and(|last| now.saturating_duration_since(last) < DEBOUNCE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debounce_window_is_250ms() {
        let base = Instant::now();
        assert!(!is_debounced(base, None));
        assert!(is_debounced(base + Duration::from_millis(249), Some(base)));
        assert!(!is_debounced(base + Duration::from_millis(250), Some(base)));
        assert!(!is_debounced(base + Duration::from_secs(1), Some(base)));
    }
}
