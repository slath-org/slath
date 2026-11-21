// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Partial landing: merging the bottom changes of a stack (up to one
//! change) while the merge request stays open with the rest. The prefix
//! lands with the same requirements as a whole merge, counted for its
//! changes.

use slath_db::merge_requests::MergeRequest;

use crate::Services;
use crate::error::{Result, ServiceError};

/// The bottom changes of the current revision that partial landing merges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefix {
    /// How many changes, counted from the bottom: at least one, fewer than
    /// the stack has.
    pub len: usize,
    /// The commit of the prefix's top change.
    pub head: String,
    /// The display IDs of its changes, bottom first.
    pub changes: Vec<String>,
}

impl Services {
    /// The prefix up to `change` (its display ID, or at least 8 of its
    /// first characters) in the current revision; `None` when it's the top
    /// change, which lands the whole merge request.
    pub async fn mr_prefix(&self, m: &MergeRequest, change: &str) -> Result<Option<Prefix>> {
        let unknown = || {
            ServiceError::invalid(
                "upto_change",
                "unknown",
                format!("The current revision has no change {change}."),
            )
        };
        let stack = self.mr_stack(m).await?;
        let ids: Vec<&str> = stack
            .entries
            .iter()
            .map(|e| e.display_id.as_deref().unwrap_or(""))
            .collect();
        let found: Vec<usize> = ids
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                !d.is_empty() && (**d == change || (change.len() >= 8 && d.starts_with(change)))
            })
            .map(|(i, _)| i)
            .collect();
        let [i] = found.as_slice() else {
            return Err(unknown());
        };
        if *i + 1 == stack.entries.len() {
            return Ok(None);
        }
        Ok(Some(Prefix {
            len: i + 1,
            head: stack.entries[*i].commit_oid.clone(),
            changes: ids[..=*i].iter().map(|d| (*d).to_owned()).collect(),
        }))
    }
}
