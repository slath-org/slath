// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Output helpers shared by the administrative subcommands. Every such
//! command can print stable JSON (`--output json`) for scripts and agents.

use std::process::ExitCode;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
}

/// Prints `value` as one line of JSON on stdout.
pub fn print_json(value: &serde_json::Value) {
    println!("{value}");
}

/// Reports a fatal error in the requested format and returns exit code 1.
pub fn fail(format: OutputFormat, message: &str) -> ExitCode {
    match format {
        OutputFormat::Text => eprintln!("slath: {message}"),
        OutputFormat::Json => print_json(&serde_json::json!({"ok": false, "error": message})),
    }
    ExitCode::FAILURE
}

/// Builds a current-thread Tokio runtime for one-shot commands.
pub fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start runtime: {e}"))
}
