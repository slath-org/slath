// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pure domain types: IDs, name validation, authorization policy (no IO).
//!
//! This crate must not depend on IO crates (database, network, filesystem,
//! processes). Everything here is deterministic apart from ID generation.
#![forbid(unsafe_code)]

pub mod authz;
pub mod change_identity;
pub mod config;
pub mod credential;
pub mod ids;
pub mod names;
pub mod net;
pub mod secret;
pub mod secret_scan;

pub use secret::Secret;
