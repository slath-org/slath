// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Security regression suite (`conf_sec_*`, `conf_rbac_*`): runs against a
//! real `slath serve` with its own database.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod auth;
mod git;
mod headers;
mod keys;
mod orgs;
mod reconcile;
mod repos;
mod settings;
mod ssh;
mod tokens;
