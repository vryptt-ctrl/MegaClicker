# Contributing to MegaClicker

Thank you for your interest in contributing to MegaClicker!

## Development Setup

1. Install [Rust stable](https://rustup.rs/) with MSVC toolchain
2. Clone the repository
3. Run `cargo build --release` to verify the build works

## Quality Gates

Before submitting a PR, ensure all checks pass:

```powershell
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

## Code Style

- Run `cargo fmt` before committing
- All clippy lints must pass with no warnings
- Add tests for new functionality
- Keep `unsafe` blocks confined to `src/input.rs` and `src/tray.rs` with
  `SAFETY:` comments

## Pull Request Process

1. Fork the repository and create your branch from `main`
2. If you've added code that should be tested, add tests
3. Ensure all CI checks pass
4. Update documentation if needed
5. Your PR will be reviewed and merged once approved

## Reporting Issues

Please include:
- Windows version (Windows 10/11)
- Rust version (`rustc --version`)
- Steps to reproduce
- Expected vs actual behavior
