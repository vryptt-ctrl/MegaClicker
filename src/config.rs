//! Configuration loading, validation, and first-run creation.
//!
//! The config file is intentionally flat so that every error can name one
//! concrete TOML key and its valid range.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

/// Default config file content. The committed `config.toml` is the source of
/// truth, so the example users see in the repository is byte-for-byte what
/// MegaClicker writes on first run.
pub const DEFAULT_CONFIG_TOML: &str = include_str!("../config.toml");

pub const DEFAULT_INTERVAL_MS: u64 = 100;
pub const MAX_INTERVAL_MS: u64 = 1_000;
pub const MAX_CLICK_LIMIT: u64 = 1_000_000_000;
pub const MAX_START_DELAY_SECONDS: u64 = 5;

const INTERVAL_EXPECTED: &str = "an integer from 1 to 1000 (milliseconds)";
const LIMIT_EXPECTED: &str = "an integer from 0 to 1000000000 (0 = unlimited)";
const DELAY_EXPECTED: &str = "an integer from 0 to 5 (seconds)";
const BUTTON_EXPECTED: &str = "one of \"left\", \"right\", \"middle\"";
const CLICK_TYPE_EXPECTED: &str = "one of \"single\", \"double\"";

/// The mouse button MegaClicker presses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

impl Button {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Middle => "middle",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "middle" => Some(Self::Middle),
            _ => None,
        }
    }
}

impl std::fmt::Display for Button {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Single press or double press per click action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickType {
    Single,
    Double,
}

impl ClickType {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Double => "double",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "single" => Some(Self::Single),
            "double" => Some(Self::Double),
            _ => None,
        }
    }
}

impl std::fmt::Display for ClickType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyKey {
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

impl HotkeyKey {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::F1 => "F1",
            Self::F2 => "F2",
            Self::F3 => "F3",
            Self::F4 => "F4",
            Self::F5 => "F5",
            Self::F6 => "F6",
            Self::F7 => "F7",
            Self::F8 => "F8",
            Self::F9 => "F9",
            Self::F10 => "F10",
            Self::F11 => "F11",
            Self::F12 => "F12",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "F1" => Some(Self::F1),
            "F2" => Some(Self::F2),
            "F3" => Some(Self::F3),
            "F4" => Some(Self::F4),
            "F5" => Some(Self::F5),
            "F6" => Some(Self::F6),
            "F7" => Some(Self::F7),
            "F8" => Some(Self::F8),
            "F9" => Some(Self::F9),
            "F10" => Some(Self::F10),
            "F11" => Some(Self::F11),
            "F12" => Some(Self::F12),
            _ => None,
        }
    }
}

impl std::fmt::Display for HotkeyKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Click behavior shared by the base configuration and every profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClickSettings {
    pub interval: Duration,
    pub button: Button,
    pub click_type: ClickType,
    /// 0 means unlimited.
    pub click_limit: u64,
    pub start_delay: Duration,
}

impl Default for ClickSettings {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(DEFAULT_INTERVAL_MS),
            button: Button::Left,
            click_type: ClickType::Single,
            click_limit: 0,
            start_delay: Duration::ZERO,
        }
    }
}

/// Validated runtime configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub interval: Duration,
    pub button: Button,
    pub click_type: ClickType,
    pub toggle_hotkey: HotkeyKey,
    pub quit_hotkey: HotkeyKey,
    pub profile_hotkey: HotkeyKey,
    /// 0 means unlimited.
    pub click_limit: u64,
    pub start_delay: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(DEFAULT_INTERVAL_MS),
            button: Button::Left,
            click_type: ClickType::Single,
            toggle_hotkey: HotkeyKey::F6,
            quit_hotkey: HotkeyKey::F8,
            profile_hotkey: HotkeyKey::F7,
            click_limit: 0,
            start_delay: Duration::ZERO,
        }
    }
}

/// Result of loading a config, including whether the file was created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loaded {
    pub config: Config,
    pub created: bool,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error(
        "APPDATA is not set; cannot locate the default config file \
         (%APPDATA%\\MegaClicker\\config.toml)"
    )]
    MissingAppData,

    #[error(
        "config file not found: {path}. Create it, pass an existing file with \
         --config, or omit --config to use the default location."
    )]
    NotFound { path: PathBuf },

    #[error("could not read the config file: {source}")]
    Read {
        #[source]
        source: io::Error,
    },

    #[error("could not create the config directory: {source}")]
    CreateDir {
        #[source]
        source: io::Error,
    },

    #[error("could not write the default config file: {source}")]
    Write {
        #[source]
        source: io::Error,
    },

    #[error("invalid TOML: {source}")]
    Parse {
        #[source]
        source: toml::de::Error,
    },

    #[error("invalid `{field}` = {value}; expected {expected}")]
    InvalidField {
        field: &'static str,
        value: String,
        expected: &'static str,
    },
}

/// Raw shape deserialized from TOML before validation. Every field has a
/// default so users can keep only the settings they care about.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default = "default_interval_ms")]
    interval_ms: i64,
    #[serde(default = "default_button")]
    button: String,
    #[serde(default = "default_click_type")]
    click_type: String,
    #[serde(default = "default_toggle_hotkey")]
    toggle_hotkey: String,
    #[serde(default = "default_quit_hotkey")]
    quit_hotkey: String,
    #[serde(default = "default_profile_hotkey")]
    profile_hotkey: String,
    #[serde(default)]
    click_limit: i64,
    #[serde(default)]
    start_delay_seconds: i64,
}

/// Raw shape of a profile file. Profiles intentionally omit hotkeys: hotkeys
/// belong to the base configuration and stay active across profile switches.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    #[serde(default = "default_interval_ms")]
    interval_ms: i64,
    #[serde(default = "default_button")]
    button: String,
    #[serde(default = "default_click_type")]
    click_type: String,
    #[serde(default)]
    click_limit: i64,
    #[serde(default)]
    start_delay_seconds: i64,
}

fn default_interval_ms() -> i64 {
    DEFAULT_INTERVAL_MS as i64
}

fn default_button() -> String {
    Button::Left.as_str().to_owned()
}

fn default_click_type() -> String {
    ClickType::Single.as_str().to_owned()
}

fn default_toggle_hotkey() -> String {
    HotkeyKey::F6.as_str().to_owned()
}

fn default_quit_hotkey() -> String {
    HotkeyKey::F8.as_str().to_owned()
}

fn default_profile_hotkey() -> String {
    HotkeyKey::F7.as_str().to_owned()
}

impl Config {
    /// Parse and validate TOML text. Kept separate from file IO so the
    /// validation rules are easy to test.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig =
            toml::from_str(text).map_err(|source| ConfigError::Parse { source })?;

        let settings = validate_click_settings(
            raw.interval_ms,
            &raw.button,
            &raw.click_type,
            raw.click_limit,
            raw.start_delay_seconds,
        )?;

        let toggle_hotkey =
            HotkeyKey::parse(&raw.toggle_hotkey).ok_or_else(|| ConfigError::InvalidField {
                field: "toggle_hotkey",
                value: raw.toggle_hotkey.clone(),
                expected: "one of F1 through F12",
            })?;
        let quit_hotkey =
            HotkeyKey::parse(&raw.quit_hotkey).ok_or_else(|| ConfigError::InvalidField {
                field: "quit_hotkey",
                value: raw.quit_hotkey.clone(),
                expected: "one of F1 through F12",
            })?;
        let profile_hotkey =
            HotkeyKey::parse(&raw.profile_hotkey).ok_or_else(|| ConfigError::InvalidField {
                field: "profile_hotkey",
                value: raw.profile_hotkey.clone(),
                expected: "one of F1 through F12",
            })?;

        validate_hotkeys(
            (toggle_hotkey, &raw.toggle_hotkey),
            (quit_hotkey, &raw.quit_hotkey),
            (profile_hotkey, &raw.profile_hotkey),
        )?;

        Ok(Self {
            interval: settings.interval,
            button: settings.button,
            click_type: settings.click_type,
            toggle_hotkey,
            quit_hotkey,
            profile_hotkey,
            click_limit: settings.click_limit,
            start_delay: settings.start_delay,
        })
    }

    /// Replace every click setting while keeping the hotkeys unchanged.
    pub fn with_click_settings(&self, settings: &ClickSettings) -> Self {
        Self {
            interval: settings.interval,
            button: settings.button,
            click_type: settings.click_type,
            click_limit: settings.click_limit,
            start_delay: settings.start_delay,
            ..*self
        }
    }
}

/// Parse the click settings used by profile files.
pub fn parse_click_settings(text: &str) -> Result<ClickSettings, ConfigError> {
    let raw: RawProfile = toml::from_str(text).map_err(|source| ConfigError::Parse { source })?;

    validate_click_settings(
        raw.interval_ms,
        &raw.button,
        &raw.click_type,
        raw.click_limit,
        raw.start_delay_seconds,
    )
}

fn validate_click_settings(
    interval_ms: i64,
    button: &str,
    click_type: &str,
    click_limit: i64,
    start_delay_seconds: i64,
) -> Result<ClickSettings, ConfigError> {
    let interval_ms = validate_integer(
        "interval_ms",
        interval_ms,
        1,
        MAX_INTERVAL_MS,
        INTERVAL_EXPECTED,
    )?;
    let click_limit = validate_integer(
        "click_limit",
        click_limit,
        0,
        MAX_CLICK_LIMIT,
        LIMIT_EXPECTED,
    )?;
    let start_delay_seconds = validate_integer(
        "start_delay_seconds",
        start_delay_seconds,
        0,
        MAX_START_DELAY_SECONDS,
        DELAY_EXPECTED,
    )?;

    let button = Button::parse(button).ok_or_else(|| ConfigError::InvalidField {
        field: "button",
        value: button.to_owned(),
        expected: BUTTON_EXPECTED,
    })?;

    let click_type = ClickType::parse(click_type).ok_or_else(|| ConfigError::InvalidField {
        field: "click_type",
        value: click_type.to_owned(),
        expected: CLICK_TYPE_EXPECTED,
    })?;

    Ok(ClickSettings {
        interval: Duration::from_millis(interval_ms),
        button,
        click_type,
        click_limit,
        start_delay: Duration::from_secs(start_delay_seconds),
    })
}

fn validate_hotkeys(
    toggle: (HotkeyKey, &str),
    quit: (HotkeyKey, &str),
    profile: (HotkeyKey, &str),
) -> Result<(), ConfigError> {
    if toggle.0 == quit.0 {
        return Err(ConfigError::InvalidField {
            field: "quit_hotkey",
            value: quit.1.to_owned(),
            expected: "a function key different from toggle_hotkey",
        });
    }
    if profile.0 == toggle.0 || profile.0 == quit.0 {
        return Err(ConfigError::InvalidField {
            field: "profile_hotkey",
            value: profile.1.to_owned(),
            expected: "a function key different from toggle_hotkey and quit_hotkey",
        });
    }

    Ok(())
}

fn validate_integer(
    field: &'static str,
    value: i64,
    min: u64,
    max: u64,
    expected: &'static str,
) -> Result<u64, ConfigError> {
    match u64::try_from(value) {
        Ok(value) if (min..=max).contains(&value) => Ok(value),
        _ => Err(ConfigError::InvalidField {
            field,
            value: value.to_string(),
            expected,
        }),
    }
}

/// `%APPDATA%\MegaClicker\config.toml`.
pub fn default_path() -> Result<PathBuf, ConfigError> {
    let app_data = env::var_os("APPDATA").ok_or(ConfigError::MissingAppData)?;
    Ok(PathBuf::from(app_data)
        .join("MegaClicker")
        .join("config.toml"))
}

/// Load an existing config file. Missing files are an error; this is used for
/// an explicitly requested `--config` path so typos are not silently replaced.
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(ConfigError::NotFound {
                path: path.to_path_buf(),
            });
        }
        Err(source) => return Err(ConfigError::Read { source }),
    };

    Config::from_toml(&text)
}

/// Load the default config, creating the directory and file with defaults on
/// first run. All IO errors are surfaced with actionable context from `main`.
pub fn load_or_create(path: &Path) -> Result<Loaded, ConfigError> {
    if path.exists() {
        return Ok(Loaded {
            config: load(path)?,
            created: false,
        });
    }

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::CreateDir { source })?;
        }
    }

    fs::write(path, DEFAULT_CONFIG_TOML).map_err(|source| ConfigError::Write { source })?;

    Ok(Loaded {
        config: Config::from_toml(DEFAULT_CONFIG_TOML)?,
        created: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Config, ConfigError> {
        Config::from_toml(text)
    }

    #[test]
    fn committed_example_matches_defaults() {
        let config = parse(DEFAULT_CONFIG_TOML).expect("committed config.toml must be valid");
        assert_eq!(config, Config::default());
    }

    #[test]
    fn empty_file_uses_defaults() {
        let config = parse("").expect("empty config must be valid");
        assert_eq!(config, Config::default());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let config = parse("button = \"right\"").expect("partial config must be valid");
        assert_eq!(config.button, Button::Right);
        assert_eq!(config.interval, Duration::from_millis(100));
        assert_eq!(config.click_type, ClickType::Single);
        assert_eq!(config.toggle_hotkey, HotkeyKey::F6);
        assert_eq!(config.quit_hotkey, HotkeyKey::F8);
        assert_eq!(config.profile_hotkey, HotkeyKey::F7);
        assert_eq!(config.click_limit, 0);
        assert_eq!(config.start_delay, Duration::ZERO);
    }

    #[test]
    fn accepts_all_valid_enum_values() {
        for button in ["left", "right", "middle", "LEFT", "Middle"] {
            let text = format!("button = \"{button}\"");
            assert!(parse(&text).is_ok(), "button {button} should be valid");
        }
        for click_type in ["single", "double", "SINGLE", "Double"] {
            let text = format!("click_type = \"{click_type}\"");
            assert!(
                parse(&text).is_ok(),
                "click type {click_type} should be valid"
            );
        }
    }

    #[test]
    fn accepts_function_key_hotkeys_case_insensitively() {
        let config =
            parse("toggle_hotkey = \"f1\"\nquit_hotkey = \"F12\"\nprofile_hotkey = \"f5\"")
                .expect("valid function keys must be accepted");
        assert_eq!(config.toggle_hotkey, HotkeyKey::F1);
        assert_eq!(config.quit_hotkey, HotkeyKey::F12);
        assert_eq!(config.profile_hotkey, HotkeyKey::F5);
    }

    #[test]
    fn rejects_invalid_or_duplicate_hotkeys() {
        for (toml, expected) in [
            ("toggle_hotkey = \"A\"", "toggle_hotkey"),
            (
                "toggle_hotkey = \"F6\"\nquit_hotkey = \"F6\"",
                "quit_hotkey",
            ),
            (
                "toggle_hotkey = \"F6\"\nquit_hotkey = \"F8\"\nprofile_hotkey = \"F6\"",
                "profile_hotkey",
            ),
            (
                "toggle_hotkey = \"F6\"\nquit_hotkey = \"F8\"\nprofile_hotkey = \"F8\"",
                "profile_hotkey",
            ),
        ] {
            let error = parse(toml).expect_err("invalid hotkeys must fail");
            assert!(error.to_string().contains(expected));
        }
    }

    #[test]
    fn profile_parser_accepts_click_settings_only() {
        let settings =
            parse_click_settings("interval_ms = 25\nbutton = \"right\"\nclick_type = \"double\"")
                .expect("valid profile settings");
        assert_eq!(settings.interval, Duration::from_millis(25));
        assert_eq!(settings.button, Button::Right);
        assert_eq!(settings.click_type, ClickType::Double);

        let error = parse_click_settings("toggle_hotkey = \"F6\"")
            .expect_err("profiles must not carry hotkeys");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn with_click_settings_keeps_hotkeys() {
        let config = parse(
            "interval_ms = 10\ntoggle_hotkey = \"F2\"\nquit_hotkey = \"F3\"\nprofile_hotkey = \"F4\"",
        )
        .expect("valid config");
        let settings = parse_click_settings("interval_ms = 500").expect("valid settings");
        let updated = config.with_click_settings(&settings);

        assert_eq!(updated.interval, Duration::from_millis(500));
        assert_eq!(updated.toggle_hotkey, HotkeyKey::F2);
        assert_eq!(updated.quit_hotkey, HotkeyKey::F3);
        assert_eq!(updated.profile_hotkey, HotkeyKey::F4);
    }

    #[test]
    fn accepts_boundary_values() {
        let config = parse("interval_ms = 1\nclick_limit = 0\nstart_delay_seconds = 0")
            .expect("lower bounds must be valid");
        assert_eq!(config.interval, Duration::from_millis(1));
        assert_eq!(config.click_limit, 0);
        assert_eq!(config.start_delay, Duration::ZERO);

        let config = parse("interval_ms = 1000\nclick_limit = 1000000000\nstart_delay_seconds = 5")
            .expect("upper bounds must be valid");
        assert_eq!(config.interval, Duration::from_millis(1000));
        assert_eq!(config.click_limit, 1_000_000_000);
        assert_eq!(config.start_delay, Duration::from_secs(5));
    }

    #[test]
    fn rejects_interval_out_of_range_with_field_and_range() {
        for value in ["0", "1001", "-5"] {
            let text = format!("interval_ms = {value}");
            let error = parse(&text).expect_err("out-of-range interval must fail");
            let message = error.to_string();
            assert!(message.contains("interval_ms"), "message: {message}");
            assert!(message.contains("1 to 1000"), "message: {message}");
            assert!(message.contains(value), "message: {message}");
        }
    }

    #[test]
    fn rejects_click_limit_out_of_range_with_field_and_range() {
        for value in ["-1", "1000000001"] {
            let text = format!("click_limit = {value}");
            let error = parse(&text).expect_err("out-of-range click limit must fail");
            let message = error.to_string();
            assert!(message.contains("click_limit"), "message: {message}");
            assert!(message.contains("0 to 1000000000"), "message: {message}");
            assert!(message.contains(value), "message: {message}");
        }
    }

    #[test]
    fn rejects_start_delay_out_of_range_with_field_and_range() {
        for value in ["-1", "6"] {
            let text = format!("start_delay_seconds = {value}");
            let error = parse(&text).expect_err("out-of-range start delay must fail");
            let message = error.to_string();
            assert!(
                message.contains("start_delay_seconds"),
                "message: {message}"
            );
            assert!(message.contains("0 to 5"), "message: {message}");
            assert!(message.contains(value), "message: {message}");
        }
    }

    #[test]
    fn rejects_unknown_fields() {
        let error = parse("interval_ms = 100\ninterval = 5").expect_err("unknown field must fail");
        let message = error.to_string();
        assert!(message.contains("interval"), "message: {message}");
        assert!(message.contains("unknown field"), "message: {message}");
    }

    #[test]
    fn rejects_invalid_button() {
        let error = parse("button = \"side\"").expect_err("invalid button must fail");
        let message = error.to_string();
        assert!(message.contains("button"), "message: {message}");
        assert!(message.contains("left"), "message: {message}");
        assert!(message.contains("side"), "message: {message}");
    }

    #[test]
    fn rejects_invalid_click_type() {
        let error = parse("click_type = \"triple\"").expect_err("invalid click type must fail");
        let message = error.to_string();
        assert!(message.contains("click_type"), "message: {message}");
        assert!(message.contains("single"), "message: {message}");
        assert!(message.contains("triple"), "message: {message}");
    }

    #[test]
    fn rejects_wrong_toml_type() {
        let error = parse("interval_ms = \"fast\"").expect_err("wrong type must fail");
        let message = error.to_string();
        assert!(message.contains("invalid TOML"), "message: {message}");
    }

    #[test]
    fn rejects_invalid_toml_syntax() {
        let error = parse("interval_ms = [").expect_err("bad TOML must fail");
        assert!(error.to_string().contains("invalid TOML"));
    }
}
