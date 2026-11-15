//! Global hotkeys: toggle clicking, switch profile, and quit.

use std::io;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::config::HotkeyKey;
use crate::input;

pub const TOGGLE_ID: i32 = 1;
pub const QUIT_ID: i32 = 2;
pub const PROFILE_ID: i32 = 3;

/// Duplicate toggle presses inside this window are ignored.
pub const DEBOUNCE: Duration = Duration::from_millis(250);

// Win32 ERROR_HOTKEY_ALREADY_REGISTERED.
const ERROR_HOTKEY_ALREADY_REGISTERED: i32 = 1409;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    Toggle,
    NextProfile,
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

/// Owns the three hotkey registrations and unregisters them on drop.
pub struct Hotkeys;

impl Hotkeys {
    pub fn register(
        toggle_key: HotkeyKey,
        quit_key: HotkeyKey,
        profile_key: HotkeyKey,
    ) -> Result<Self, HotkeyError> {
        if let Err(error) = input::register_hotkey(TOGGLE_ID, toggle_key) {
            return Err(map_error(toggle_key, error));
        }

        if let Err(error) = input::register_hotkey(QUIT_ID, quit_key) {
            let _ = input::unregister_hotkey(TOGGLE_ID);
            return Err(map_error(quit_key, error));
        }

        if let Err(error) = input::register_hotkey(PROFILE_ID, profile_key) {
            let _ = input::unregister_hotkey(TOGGLE_ID);
            let _ = input::unregister_hotkey(QUIT_ID);
            return Err(map_error(profile_key, error));
        }

        Ok(Self)
    }

    /// Wait up to `timeout` for the next hotkey press.
    pub fn wait_for_event(&self, timeout: Duration) -> Option<HotkeyAction> {
        match input::wait_for_hotkey(timeout)? {
            TOGGLE_ID => Some(HotkeyAction::Toggle),
            QUIT_ID => Some(HotkeyAction::Quit),
            PROFILE_ID => Some(HotkeyAction::NextProfile),
            _ => None,
        }
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        let _ = input::unregister_hotkey(TOGGLE_ID);
        let _ = input::unregister_hotkey(QUIT_ID);
        let _ = input::unregister_hotkey(PROFILE_ID);
    }
}

fn map_error(key: HotkeyKey, error: io::Error) -> HotkeyError {
    if error.raw_os_error() == Some(ERROR_HOTKEY_ALREADY_REGISTERED) {
        HotkeyError::Conflict { key: key.as_str() }
    } else {
        HotkeyError::Register {
            key: key.as_str(),
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
