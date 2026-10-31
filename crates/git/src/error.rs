// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Errors from Git operations.

use std::process::ExitStatus;

/// Which limit an operation ran into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    Timeout,
    Output,
}

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// The process was killed because it exceeded a limit.
    #[error("git operation exceeded its {0:?} limit")]
    Limit(LimitKind),
    /// Git exited unsuccessfully. `stderr` is for logs only and must not be
    /// returned to clients.
    #[error("git exited with {status}")]
    Failed { status: ExitStatus, stderr: Vec<u8> },
    #[error("object not found")]
    NotFound,
    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),
    #[error("unexpected git output: {0}")]
    Protocol(&'static str),
    #[error("unsupported git version {found}; Slath needs {required} or later")]
    UnsupportedVersion { found: String, required: String },
    #[error("git runner is shutting down")]
    Closed,
    #[error("i/o error running git: {0}")]
    Io(#[from] std::io::Error),
}
