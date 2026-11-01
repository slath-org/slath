// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Notifications about issues (and later merge requests).

use slath_core::authz::Subject;
use slath_db::issues::Issue;

use crate::Services;
use crate::repos::RepoAccess;

/// What happened to an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Opened,
    Closed,
    Reopened,
    Commented(uuid::Uuid),
}

impl Services {
    /// Tells the item's subscribers (filled in with subscriptions and the
    /// inbox).
    pub(crate) async fn notify_item(
        &self,
        _subject: &Subject,
        _a: &RepoAccess,
        _issue: &Issue,
        _what: Activity,
    ) {
    }
}
