//! Single-instance enforcement using a named Win32 mutex.
//!
//! The mutex name is scoped to the current Windows session (`Local\`), so each
//! logged-in user can run one copy without requiring administrator rights.

use thiserror::Error;

use crate::input::NamedMutex;

const INSTANCE_MUTEX_NAME: &str = "Local\\MegaClicker.SingleInstance.v1";

#[derive(Debug, Error)]
pub enum InstanceError {
    #[error(
        "MegaClicker is already running in this Windows session. Only one copy \
         can run at a time. Switch to the existing console and press F8 to quit \
         it before starting another copy."
    )]
    AlreadyRunning,

    #[error("could not create the single-instance lock: {0}")]
    Lock(#[source] std::io::Error),
}

/// Holds the named mutex for the lifetime of the process.
pub struct InstanceGuard {
    _mutex: NamedMutex,
}

impl InstanceGuard {
    pub fn acquire() -> Result<Self, InstanceError> {
        match NamedMutex::try_acquire(INSTANCE_MUTEX_NAME).map_err(InstanceError::Lock)? {
            Some(mutex) => Ok(Self { _mutex: mutex }),
            None => Err(InstanceError::AlreadyRunning),
        }
    }
}
