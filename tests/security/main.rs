// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Security regression suite (`conf_sec_*`, `conf_rbac_*`): runs against a
//! real `slath serve` with its own database.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod admin;
mod api;
mod attachments;
mod auth;
mod browse;
mod email;
mod git;
mod headers;
mod issues;
mod keys;
mod m2_surfaces;
mod mfa;
mod notifications;
mod oidc;
mod orgs;
mod passkeys;
mod protection;
mod reconcile;
mod recovery;
mod redaction;
mod references;
mod registration;
mod repos;
mod routes;
mod search;
mod settings;
mod ssh;
mod surfaces;
mod teams;
mod tokens;
mod updates;
mod vuln_git;
mod vuln_m2;
mod vuln_rbac;
mod vuln_web;
