//! Named click profiles stored as individual TOML files.
//!
//! A profile contains click settings only. Hotkeys belong to the base
//! configuration, so switching a profile never has to re-register hotkeys.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::config::{self, ClickSettings, Config, ConfigError};

/// Name of the implicit profile backed by the base config file.
pub const DEFAULT_PROFILE: &str = "default";

/// Folder created next to the base config file.
pub const PROFILES_DIR: &str = "profiles";

pub const MAX_NAME_LEN: usize = 32;

/// A named set of click settings loaded from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub settings: ClickSettings,
}

/// The profile chosen by one step of the cycle hotkey.
#[derive(Debug)]
pub enum Selection {
    /// The base configuration's click settings.
    Default,
    /// A named profile loaded from the profiles folder.
    Named(Profile),
}

impl Selection {
    pub fn name(&self) -> &str {
        match self {
            Self::Default => DEFAULT_PROFILE,
            Self::Named(profile) => &profile.name,
        }
    }

    /// Build a runtime config by applying this selection to the base config.
    pub fn apply(&self, base: &Config) -> Config {
        match self {
            Self::Default => *base,
            Self::Named(profile) => base.with_click_settings(&profile.settings),
        }
    }
}

#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("invalid profile name `{name}`: {reason}")]
    InvalidName { name: String, reason: &'static str },

    #[error("could not create the profile directory: {source}")]
    CreateDir {
        #[source]
        source: io::Error,
    },

    #[error("could not read profile `{name}`: {source}")]
    Read {
        name: String,
        #[source]
        source: io::Error,
    },

    #[error("could not write profile `{name}`: {source}")]
    Write {
        name: String,
        #[source]
        source: io::Error,
    },

    #[error("invalid profile `{name}`: {source}")]
    Invalid {
        name: String,
        #[source]
        source: ConfigError,
    },

    #[error("could not list profiles in {directory}: {source}")]
    List {
        directory: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Profiles live next to the base config so `--config` keeps everything in one
/// place.
pub fn directory(config_path: &Path) -> PathBuf {
    match config_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(PROFILES_DIR),
        _ => PathBuf::from(PROFILES_DIR),
    }
}

/// Validate a profile name before it is used as a file name.
pub fn validate_name(name: &str) -> Result<(), ProfileError> {
    let invalid = |reason: &'static str| ProfileError::InvalidName {
        name: name.to_owned(),
        reason,
    };

    if name.is_empty() {
        return Err(invalid("the name is empty"));
    }
    if name.trim() != name {
        return Err(invalid("the name has leading or trailing whitespace"));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(invalid("the name is longer than 32 characters"));
    }
    if name.eq_ignore_ascii_case(DEFAULT_PROFILE) {
        return Err(invalid("`default` is reserved for the base configuration"));
    }
    if !name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
    {
        return Err(invalid("use only ASCII letters, digits, '-' and '_'"));
    }

    Ok(())
}

/// Full path of a named profile file.
pub fn path_for(config_path: &Path, name: &str) -> Result<PathBuf, ProfileError> {
    validate_name(name)?;
    Ok(directory(config_path).join(format!("{name}.toml")))
}

/// Load an existing profile. Missing files are an error; use
/// [`load_or_create`] when a missing profile should be initialized.
pub fn load(config_path: &Path, name: &str) -> Result<Profile, ProfileError> {
    let path = path_for(config_path, name)?;
    let text = fs::read_to_string(&path).map_err(|source| ProfileError::Read {
        name: name.to_owned(),
        source,
    })?;

    parse(name, &text)
}

/// Load a profile, writing a commented default file when it does not exist.
pub fn load_or_create(config_path: &Path, name: &str) -> Result<Profile, ProfileError> {
    let path = path_for(config_path, name)?;

    if path.exists() {
        return load(config_path, name);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ProfileError::CreateDir { source })?;
    }

    let template = template(name);
    fs::write(&path, &template).map_err(|source| ProfileError::Write {
        name: name.to_owned(),
        source,
    })?;

    parse(name, &template)
}

fn parse(name: &str, text: &str) -> Result<Profile, ProfileError> {
    let settings = config::parse_click_settings(text).map_err(|source| ProfileError::Invalid {
        name: name.to_owned(),
        source,
    })?;

    Ok(Profile {
        name: name.to_owned(),
        settings,
    })
}

/// Named profiles found on disk, sorted case-insensitively. Invalid file names
/// are ignored because they cannot be selected from the CLI anyway.
pub fn list(config_path: &Path) -> Result<Vec<String>, ProfileError> {
    let directory = directory(config_path);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(ProfileError::List { directory, source }),
    };

    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ProfileError::List {
            directory: directory.clone(),
            source,
        })?;
        let path = entry.path();

        if !path.is_file() {
            continue;
        }
        if !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"))
        {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            if validate_name(stem).is_ok() {
                names.push(stem.to_owned());
            }
        }
    }

    names.sort_by_key(|name| name.to_ascii_lowercase());
    Ok(names)
}

#[derive(Debug)]
pub struct CycleOutcome {
    pub selection: Selection,
    /// Invalid profiles that were skipped while looking for the next one.
    pub skipped: Vec<String>,
}

/// Advance to the next profile after `current`, wrapping around. Broken
/// profile files are skipped and reported instead of stopping the cycle.
pub fn cycle(config_path: &Path, current: &str) -> Result<CycleOutcome, ProfileError> {
    let mut names = vec![DEFAULT_PROFILE.to_owned()];
    names.extend(list(config_path)?);

    let count = names.len();
    let start = names
        .iter()
        .position(|name| name.eq_ignore_ascii_case(current))
        .map_or(0, |index| (index + 1) % count);
    let mut skipped = Vec::new();

    for offset in 0..count {
        let name = &names[(start + offset) % count];

        if name.eq_ignore_ascii_case(DEFAULT_PROFILE) {
            return Ok(CycleOutcome {
                selection: Selection::Default,
                skipped,
            });
        }

        match load(config_path, name) {
            Ok(profile) => {
                return Ok(CycleOutcome {
                    selection: Selection::Named(profile),
                    skipped,
                })
            }
            Err(error) => skipped.push(error.to_string()),
        }
    }

    // Only the base configuration is usable.
    Ok(CycleOutcome {
        selection: Selection::Default,
        skipped,
    })
}

fn template(name: &str) -> String {
    format!(
        r#"# MegaClicker profile `{name}`.
#
# Profiles hold click settings only. Hotkeys come from config.toml and stay
# active when profiles are switched. Missing values fall back to the defaults.

# Time between the start of consecutive clicks, in milliseconds.
# Allowed: 1-1000
interval_ms = 100

# Randomize each interval by up to this percentage. 0 disables randomization.
# Allowed: 0-50
interval_jitter_percent = 0

# Mouse button to click.
# Allowed: "left", "right", "middle"
button = "left"

# Click type. "double" sends two rapid button presses per click action and
# still counts as ONE click action towards click_limit.
# Allowed: "single", "double"
click_type = "single"

# Before each click, move the cursor to a random point within this many
# pixels of where clicking started. 0 keeps the cursor in place.
# Allowed: 0-100
cursor_jitter_px = 0

# Stop automatically after this many click actions. 0 means unlimited.
# Allowed: 0-1000000000
click_limit = 0

# Delay before the first click after each start, in seconds.
# Allowed: 0-5
start_delay_seconds = 0
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    /// Each test gets its own config path inside a fresh temp directory.
    fn temp_config_path() -> PathBuf {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::SeqCst);
        let directory =
            std::env::temp_dir().join(format!("megaclicker-test-{}-{id}", std::process::id()));
        fs::create_dir_all(&directory).expect("create temp directory");
        directory.join("config.toml")
    }

    #[test]
    fn rejects_unsafe_profile_names() {
        for name in [
            "",
            " default ",
            "default",
            "a/b",
            "a\\b",
            "..",
            "has space",
            "ü",
        ] {
            assert!(
                validate_name(name).is_err(),
                "name {name:?} should be rejected"
            );
        }
        for name in ["gaming", "work_2026", "quick-test", "A1"] {
            assert!(
                validate_name(name).is_ok(),
                "name {name:?} should be accepted"
            );
        }
    }

    #[test]
    fn creates_defaults_on_first_use_and_loads_them_back() {
        let config_path = temp_config_path();
        let profile = load_or_create(&config_path, "gaming").expect("create profile");

        assert_eq!(profile.name, "gaming");
        assert_eq!(profile.settings, ClickSettings::default());

        let path = path_for(&config_path, "gaming").expect("profile path");
        assert!(path.exists());
        assert_eq!(load(&config_path, "gaming").expect("reload"), profile);
    }

    #[test]
    fn lists_profiles_sorted_and_ignores_invalid_names() {
        let config_path = temp_config_path();
        load_or_create(&config_path, "work").expect("create work");
        load_or_create(&config_path, "Gaming").expect("create Gaming");

        let directory = directory(&config_path);
        fs::write(directory.join("bad name.toml"), "interval_ms = 50\n")
            .expect("write invalid name");
        fs::write(directory.join("notes.txt"), "ignored").expect("write non-profile");

        assert_eq!(list(&config_path).expect("list"), vec!["Gaming", "work"]);
    }

    #[test]
    fn rejects_profile_with_hotkeys_or_bad_values() {
        let config_path = temp_config_path();
        let path = path_for(&config_path, "broken").expect("profile path");
        fs::create_dir_all(path.parent().expect("parent")).expect("create profiles dir");
        fs::write(&path, "toggle_hotkey = \"F6\"\n").expect("write profile");

        let error = load(&config_path, "broken").expect_err("hotkey field must be rejected");
        assert!(error.to_string().contains("invalid profile `broken`"));
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn cycle_wraps_and_skips_broken_profiles() {
        let config_path = temp_config_path();
        load_or_create(&config_path, "alpha").expect("create alpha");
        load_or_create(&config_path, "broken").expect("create broken");
        load_or_create(&config_path, "zulu").expect("create zulu");

        fs::write(
            path_for(&config_path, "broken").expect("broken path"),
            "interval_ms = 0\n",
        )
        .expect("break profile");

        let outcome = cycle(&config_path, DEFAULT_PROFILE).expect("cycle");
        assert_eq!(outcome.selection.name(), "alpha");
        assert!(outcome.skipped.is_empty());

        let outcome = cycle(&config_path, "alpha").expect("cycle");
        assert_eq!(outcome.selection.name(), "zulu");
        assert_eq!(outcome.skipped.len(), 1);
        assert!(outcome.skipped[0].contains("broken"));

        let outcome = cycle(&config_path, "zulu").expect("cycle");
        assert_eq!(outcome.selection.name(), DEFAULT_PROFILE);
    }

    #[test]
    fn cycle_stays_on_default_when_no_profile_is_usable() {
        let config_path = temp_config_path();
        load_or_create(&config_path, "broken").expect("create broken");
        fs::write(
            path_for(&config_path, "broken").expect("broken path"),
            "interval_ms = -1\n",
        )
        .expect("break profile");

        let outcome = cycle(&config_path, DEFAULT_PROFILE).expect("cycle");
        assert_eq!(outcome.selection.name(), DEFAULT_PROFILE);
        assert_eq!(outcome.skipped.len(), 1);
    }
}
