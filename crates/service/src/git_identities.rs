// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The accounts behind the email addresses written into commits.
//!
//! An author or committer is shown as an account when the commit's address
//! is one the account confirmed, or its no-reply address
//! (`<name>@users.noreply.<host>`, which Slath writes into the commits it
//! makes for someone). Names and unconfirmed addresses never match, and
//! deleted accounts don't either. A match says whose address it is, not
//! who made the commit. Commits a bot's push brought are the bot's: Slath
//! knows who pushed them.

use std::collections::HashMap;

use slath_core::authz::{Action, Subject};
use slath_core::ids::RepoId;

use crate::{Result, Services};

/// A commit's author and committer as written into it.
#[derive(Debug, Clone, Copy)]
pub struct CommitIdent<'a> {
    pub oid: &'a str,
    pub author_email: &'a str,
    /// Empty where the page doesn't show the committer.
    pub committer_email: &'a str,
}

/// The accounts a page shows for a commit; empty where none matches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitPeople {
    pub author: String,
    pub committer: String,
}

/// An account an address belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Owner {
    name: String,
    bot: bool,
}

impl Services {
    /// The accounts to show for each commit of a repository the subject
    /// can read, in order. A commit a bot's push brought shows the bot as
    /// its committer, and as its author unless the author address is a
    /// person's; any other commit shows the accounts its addresses belong
    /// to.
    pub async fn commit_people(
        &self,
        subject: &Subject,
        repo: RepoId,
        commits: &[CommitIdent<'_>],
    ) -> Result<Vec<CommitPeople>> {
        self.repo_access_by_id(subject, repo, Action::RepoRead)
            .await?;
        if commits.is_empty() {
            return Ok(Vec::new());
        }
        let owners = self
            .owners_of(
                commits
                    .iter()
                    .flat_map(|c| [c.author_email, c.committer_email]),
            )
            .await?;
        let oids: Vec<String> = commits.iter().map(|c| c.oid.to_owned()).collect();
        let bots: HashMap<String, String> = slath_db::bot_commits::bots(&self.pool, repo, &oids)
            .await?
            .into_iter()
            .collect();
        let owner = |e: &str| owners.get(&e.trim().to_lowercase());
        Ok(commits
            .iter()
            .map(|c| {
                let author = owner(c.author_email);
                let committer = owner(c.committer_email);
                match bots.get(c.oid) {
                    Some(bot) => CommitPeople {
                        author: match author {
                            Some(o) if !o.bot => o.name.clone(),
                            _ => bot.clone(),
                        },
                        committer: bot.clone(),
                    },
                    None => CommitPeople {
                        author: author.map(|o| o.name.clone()).unwrap_or_default(),
                        committer: committer.map(|o| o.name.clone()).unwrap_or_default(),
                    },
                }
            })
            .collect())
    }

    /// The account each of `emails` belongs to, keyed by the address in
    /// lower case. Addresses without an account are left out.
    async fn owners_of<'a>(
        &self,
        emails: impl IntoIterator<Item = &'a str>,
    ) -> Result<HashMap<String, Owner>> {
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
        let mut out: HashMap<String, Owner> = HashMap::new();
        if !confirmed.is_empty() {
            for (e, name, bot) in
                slath_db::users::names_by_confirmed_email(&self.pool, &confirmed).await?
            {
                out.insert(e, Owner { name, bot });
            }
        }
        if !noreply.is_empty() {
            let names: Vec<String> = noreply.keys().cloned().collect();
            for (name, bot) in slath_db::users::existing_names(&self.pool, &names).await? {
                for e in noreply.remove(&name.to_lowercase()).unwrap_or_default() {
                    out.insert(
                        e,
                        Owner {
                            name: name.clone(),
                            bot,
                        },
                    );
                }
            }
        }
        Ok(out)
    }
}
