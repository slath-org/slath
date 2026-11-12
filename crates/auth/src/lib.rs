// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Passwords, sessions, tokens, and authentication primitives.
#![forbid(unsafe_code)]

pub mod crypto;
pub mod csrf;
pub mod master_key;
pub mod oidc;
pub mod password;
pub mod ssh_key;
pub mod token;
pub mod totp;
pub mod webhook;

pub use crypto::Envelope;
pub use master_key::MasterKey;
