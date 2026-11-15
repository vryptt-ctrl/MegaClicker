//! Global hotkeys: toggle clicking, switch profile, and quit.

use std::io;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::config::HotkeyKey;
use crate::input;

pub const TOGGLE_ID: i32 = 1;
pub const QUIT_ID: i32 = 2;
pub const PROFILE_ID: i32 = 3;

/// Hotkey ids in the same order as [`Hotkeys::keys`].
const HOTKEY_IDS: [i32; 3] = [TOGGLE_ID, QUIT_ID, PROFILE_ID];

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
         {key}, then try again."
    )]
    Conflict { key: &'static str },

    #[error("cannot register {key} as a global hotkey: {details}")]
    Register { key: &'static str, details: String },
}

/// Owns the three hotkey registrations and unregisters them on drop.
pub struct Hotkeys {
    keys: [HotkeyKey; 3],
}

impl Hotkeys {
    pub fn register(
        toggle_key: HotkeyKey,
        quit_key: HotkeyKey,
        profile_key: HotkeyKey,
    ) -> Result<Self, HotkeyError> {
        let keys = [toggle_key, quit_key, profile_key];
        let mut registered: Vec<i32> = Vec::with_capacity(HOTKEY_IDS.len());

        for (index, key) in keys.iter().enumerate() {
            match input::register_hotkey(HOTKEY_IDS[index], *key) {
                Ok(()) => registered.push(HOTKEY_IDS[index]),
                Err(error) => {
                    unregister_ids(&registered);
                    return Err(map_error(*key, error));
                }
            }
        }

        Ok(Self { keys })
    }

    /// Keys currently bound, in toggle/quit/profile order.
    pub const fn keys(&self) -> [HotkeyKey; 3] {
        self.keys
    }

    /// Point the three hotkeys at new keys. Keys that do not change keep their
    /// existing registration. If any new key is unavailable, changed keys are
    /// best-effort restored to the previous set and the error is returned.
    pub fn rebind(&mut self, keys: [HotkeyKey; 3]) -> Result<(), HotkeyError> {
        if keys == self.keys {
            return Ok(());
        }

        let changed: [bool; 3] = std::array::from_fn(|index| keys[index] != self.keys[index]);
        let mut registered: Vec<i32> = Vec::with_capacity(HOTKEY_IDS.len());

        for (index, id) in HOTKEY_IDS.iter().enumerate() {
            if changed[index] {
                let _ = input::unregister_hotkey(*id);
            }
        }

        for (index, key) in keys.iter().enumerate() {
            if !changed[index] {
                continue;
            }

            match input::register_hotkey(HOTKEY_IDS[index], *key) {
                Ok(()) => registered.push(HOTKEY_IDS[index]),
                Err(error) => {
                    unregister_ids(&registered);
                    // The old keys were free moments ago; this is best effort
                    // because a competing application could take one in between.
                    for (index, id) in HOTKEY_IDS.iter().enumerate() {
                        if changed[index] {
                            let _ = input::register_hotkey(*id, self.keys[index]);
                        }
                    }
                    return Err(map_error(*key, error));
                }
            }
        }

        self.keys = keys;
        Ok(())
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
        for id in HOTKEY_IDS {
            let _ = input::unregister_hotkey(id);
        }
    }
}

/// Unregister every id a failed registration loop left behind.
fn unregister_ids(ids: &[i32]) {
    for id in ids {
        let _ = input::unregister_hotkey(*id);
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
