use std::path::PathBuf;

use clap::Parser;

/// Command-line interface for MegaClicker.
#[derive(Debug, Parser)]
#[command(
    name = "megaclicker",
    version,
    about = "A very light but very powerful auto clicker for Windows",
    long_about = None
)]
pub struct Cli {
    /// Path to a TOML config file (defaults to %APPDATA%\MegaClicker\config.toml).
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Count clicks and follow the real timing without sending mouse input.
    #[arg(long)]
    pub dry_run: bool,
}
