// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Git and jj client compatibility suite (`conf_git_*`, `conf_jj_*`).
//! Needs the `slath` binary built and a Git client (`$SLATH_TEST_GIT`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod auto_merge;
mod change_identity;
mod cross_references;
mod deploy_keys;
mod fork_merge_requests;
mod forks;
mod git_http;
mod git_ssh;
mod import;
mod jj;
mod ledger;
mod merge;
mod merge_requests;
mod mergeability;
mod oidc_providers;
mod reserved_refs;
mod reviews;
mod threads;
mod update_branch;
