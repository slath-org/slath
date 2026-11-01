// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Operations suite (`conf_ops_*`): backup, restore, and maintenance against
//! real instances.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod backup;
mod compose;
mod fixture;
mod maintenance;
