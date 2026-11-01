// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Passwords, sessions, tokens, and authentication primitives.
#![forbid(unsafe_code)]

pub mod csrf;
pub mod master_key;
pub mod password;
pub mod token;

pub use master_key::MasterKey;
