// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Git access for Slath.
//!
//! All Git processes are spawned by [`runner::Runner`]. No other code in the
//! workspace may run `git` (enforced by `scripts/guards.sh`). User-supplied
//! revisions, paths, and ref names reach Git only through the validated types
//! in [`args`].
#![forbid(unsafe_code)]

pub mod args;
pub mod commit;
pub mod error;
pub mod hookipc;
pub mod object;
pub mod runner;
pub mod ssh;
pub mod storage;
pub mod transport;
pub mod version;

pub use error::{GitError, LimitKind};
pub use object::{ObjectId, ObjectReader};
pub use runner::{Runner, RunnerConfig};
