// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Slath's predicted jj IDs against real jj binaries.
//!
//! The server never runs jj: it computes the ID jj gives a commit without
//! a `change-id` header itself. Here every jj binary under test (the pinned
//! versions, and nightly also the latest release) imports a corpus of such
//! commits, every fixture generator's history, and each ID jj shows must
//! be the prediction. If jj changes its rule, this fails until Slath has a
//! matching variant.

use std::path::{Path, PathBuf};

use slath_tests::fixtures;
use slath_tests::server::GitClient;

use crate::jj::{Jj, jj_binaries};

/// A commit whose jj ID isn't the prediction.
pub(crate) struct Mismatch {
    oid: String,
    jj: String,
    predicted: Option<String>,
}

impl std::fmt::Debug for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let predicted = self.predicted.as_deref().unwrap_or("none");
        write!(f, "{}: jj {}, predicted {predicted}", self.oid, self.jj)
    }
}

/// The pairs of `(commit ID, jj's change ID)` where `predict` disagrees.
pub(crate) fn mismatches(
    ids: &[(String, String)],
    predict: impl Fn(&str) -> Option<String>,
) -> Vec<Mismatch> {
    ids.iter()
        .filter_map(|(oid, jj)| {
            let predicted = predict(oid);
            (predicted.as_deref() != Some(jj.as_str())).then(|| Mismatch {
                oid: oid.clone(),
                jj: jj.clone(),
                predicted,
            })
        })
        .collect()
}

/// A bare repository with every fixture's branches and tags, each under
/// the fixture's name. None of these commits has a `change-id` header.
fn corpus(git: &GitClient, dir: &Path) -> PathBuf {
    let bare = dir.join("corpus.git");
    git.ok(dir, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    for name in fixtures::names() {
        let work = dir.join(&name);
        let args: &[&str] = if name == "deep_history" {
            &["300"]
        } else {
            &[]
        };
        fixtures::generate(&name, &work, args);
        git.ok(
            &bare,
            &[
                "fetch",
                "-q",
                work.to_str().unwrap(),
                &format!("+refs/heads/*:refs/heads/{name}/*"),
                &format!("+refs/tags/*:refs/tags/{name}/*"),
            ],
        );
    }
    let raw = git.ok(&bare, &["rev-list", "--all", "--header"]);
    for commit in raw.split('\0').filter(|c| !c.trim().is_empty()) {
        let headers = commit.split("\n\n").next().unwrap_or_default();
        assert!(
            !headers.lines().any(|l| l.starts_with("change-id ")),
            "corpus commit with a header:\n{commit}"
        );
    }
    bare
}

/// Every commit jj imports from `corpus`, with the ID jj gives it. jj's
/// own working-copy commit is left out.
fn jj_ids(jj: &Jj, corpus: &Path, dir: &Path) -> Vec<(String, String)> {
    let repo = dir.join("jj");
    jj.ok(
        dir,
        &[
            "git",
            "init",
            &format!("--git-repo={}", corpus.display()),
            repo.to_str().unwrap(),
        ],
    );
    jj.ok(
        &repo,
        &[
            "log",
            "--ignore-working-copy",
            "--no-graph",
            "-r",
            "all() ~ @",
            "-T",
            r#"commit_id ++ " " ++ change_id ++ "\n""#,
        ],
    )
    .lines()
    .map(|l| {
        let (oid, id) = l.split_once(' ').unwrap();
        (oid.to_owned(), id.to_owned())
    })
    .collect()
}

fn commit_count(git: &GitClient, corpus: &Path) -> usize {
    let out = git.ok(corpus, &["rev-list", "--all", "--count"]);
    out.trim().parse().unwrap()
}

#[test]
fn conf_jj_09_predicted_ids_match_every_jj_release() {
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let corpus = corpus(&git, d.path());
    let count = commit_count(&git, &corpus);
    assert!(count >= 300, "corpus too small: {count}");
    for (version, bin) in jj_binaries() {
        let dir = tempfile::tempdir().unwrap();
        let jj = Jj::new(&version, &bin);
        let ids = jj_ids(&jj, &corpus, dir.path());
        // Every commit, and jj's root commit.
        assert_eq!(ids.len(), count + 1, "{version}: commits jj imported");
        let wrong = mismatches(&ids, slath_core::change_identity::predict);
        assert!(
            wrong.is_empty(),
            "{version}: {} of {} predictions differ from jj, first: {:#?}",
            wrong.len(),
            ids.len(),
            &wrong[..wrong.len().min(5)]
        );
    }
}

/// The check catches a rule that differs from jj's: here the prediction
/// without reversing each byte's bits.
#[test]
fn a_wrong_prediction_fails_the_check() {
    let Some((version, bin)) = jj_binaries().into_iter().last() else {
        eprintln!("no jj: skipping");
        return;
    };
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let corpus = corpus(&git, d.path());
    let jj = Jj::new(&version, &bin);
    let ids = jj_ids(&jj, &corpus, d.path());
    let wrong = |oid: &str| {
        // jj's alphabet, bytes 4..20 reversed, bits left as they are.
        let bytes = (0..20).map(|i| u8::from_str_radix(&oid[2 * i..2 * i + 2], 16).unwrap());
        let mut id = [0u8; 16];
        for (slot, b) in id
            .iter_mut()
            .zip(bytes.skip(4).collect::<Vec<_>>().into_iter().rev())
        {
            *slot = b;
        }
        Some(slath_core::change_identity::id_from_bytes(&id))
    };
    let found = mismatches(&ids, wrong);
    // Only the root commit (all zeros) agrees.
    assert_eq!(found.len(), ids.len() - 1, "{version}");
    assert!(mismatches(&ids, slath_core::change_identity::predict).is_empty());
}
