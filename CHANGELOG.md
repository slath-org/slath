# Changelog

All notable changes to Slath are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Before 1.0.0, minor versions may contain breaking changes.

## [Unreleased]

### Added

- Cargo workspace (edition 2024) with the `core`, `db`, `git`, `markup`, `auth`,
  `service`, `http`, and `ci` crates and the `slath` binary, workspace lints
  (`unsafe_code` forbidden, `unwrap`/`expect` denied outside tests), and a
  `justfile` whose `just check` runs formatting, Clippy, and tests.
