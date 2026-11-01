// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Security regression suite (`conf_sec_*`, `conf_rbac_*`): runs against a
//! real `slath serve` with its own database.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod admin;
mod api;
mod auth;
mod browse;
mod email;
mod git;
mod headers;
mod keys;
mod mfa;
mod orgs;
mod passkeys;
mod reconcile;
mod recovery;
mod redaction;
mod registration;
mod repos;
mod routes;
mod settings;
mod ssh;
mod surfaces;
mod teams;
mod tokens;
mod vuln_git;
mod vuln_rbac;
mod vuln_web;
