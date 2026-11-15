// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The accounts behind the email addresses written into commits.
//!
//! An author or committer is shown as an account when the commit's address
//! is one the account confirmed, or its no-reply address
//! (`<name>@users.noreply.<host>`, which Slath writes into the commits it
//! makes for someone). Names and unconfirmed addresses never match, and
//! deleted accounts don't either. A match says whose address it is, not
//! who made the commit.

use std::collections::HashMap;

use crate::{Result, Services};

impl Services {
    /// The account name for each of `emails` that belongs to one, keyed
    /// by the address in lower case. Addresses without an account are
    /// left out.
    pub async fn accounts_for_emails<'a>(
        &self,
        emails: impl IntoIterator<Item = &'a str>,
    ) -> Result<HashMap<String, String>> {
        let suffix =
            format!("@users.noreply.{}", self.config.server.public_url.host()).to_lowercase();
        let mut confirmed: Vec<String> = Vec::new();
        let mut noreply: HashMap<String, Vec<String>> = HashMap::new();
        for e in emails {
            let e = e.trim().to_lowercase();
            if e.is_empty() {
                continue;
            }
            match e.strip_suffix(&suffix) {
                Some(local) if !local.is_empty() && !local.contains('@') => {
                    noreply.entry(local.to_owned()).or_default().push(e.clone());
                }
                _ => confirmed.push(e),
            }
        }
        confirmed.sort();
        confirmed.dedup();
        let mut out: HashMap<String, String> = HashMap::new();
        if !confirmed.is_empty() {
            out.extend(slath_db::users::names_by_confirmed_email(&self.pool, &confirmed).await?);
        }
        if !noreply.is_empty() {
            let names: Vec<String> = noreply.keys().cloned().collect();
            for name in slath_db::users::existing_names(&self.pool, &names).await? {
                for e in noreply.remove(&name.to_lowercase()).unwrap_or_default() {
                    out.insert(e, name.clone());
                }
            }
        }
        Ok(out)
    }
}
