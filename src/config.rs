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

/// Validated runtime configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub interval: Duration,
    pub button: Button,
    pub click_type: ClickType,
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

impl Config {
    /// Parse and validate TOML text. Kept separate from file IO so the
    /// validation rules are easy to test.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig =
            toml::from_str(text).map_err(|source| ConfigError::Parse { source })?;

        let interval_ms = validate_integer(
            "interval_ms",
            raw.interval_ms,
            1,
            MAX_INTERVAL_MS,
            INTERVAL_EXPECTED,
        )?;
        let click_limit = validate_integer(
            "click_limit",
            raw.click_limit,
            0,
            MAX_CLICK_LIMIT,
            LIMIT_EXPECTED,
        )?;
        let start_delay_seconds = validate_integer(
            "start_delay_seconds",
            raw.start_delay_seconds,
            0,
            MAX_START_DELAY_SECONDS,
            DELAY_EXPECTED,
        )?;

        let button = Button::parse(&raw.button).ok_or_else(|| ConfigError::InvalidField {
            field: "button",
            value: raw.button.clone(),
            expected: BUTTON_EXPECTED,
        })?;

        let click_type =
            ClickType::parse(&raw.click_type).ok_or_else(|| ConfigError::InvalidField {
                field: "click_type",
                value: raw.click_type.clone(),
                expected: CLICK_TYPE_EXPECTED,
            })?;

        Ok(Self {
            interval: Duration::from_millis(interval_ms),
            button,
            click_type,
            click_limit,
            start_delay: Duration::from_secs(start_delay_seconds),
        })
    }
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
