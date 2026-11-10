// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mergeability API, the merge box, and the merge itself agree
//! (REVIEW-10): over random branch protection settings and merge request
//! states, each case's unmet requirements match what the settings and the
//! state say, the merge box shows each requirement's reason and offers
//! merging exactly when the API says it's mergeable, and merging succeeds
//! exactly then.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

use crate::fork_merge_requests::{api, commit, eventually, token};

/// A small deterministic generator (xorshift), so a failing case can be
/// rerun with its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn bool(&mut self) -> bool {
        self.next().is_multiple_of(2)
    }
    fn pick(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[derive(Debug)]
struct Case {
    required_approvals: i32,
    require_up_to_date: bool,
    require_resolved_threads: bool,
    require_ci: bool,
    draft: bool,
    /// Bob's verdict: none, approve, request_changes.
    verdict: Option<&'static str>,
    /// Carol's status on the head: none, failure, success.
    status: Option<&'static str>,
    behind: bool,
    unresolved_thread: bool,
}

impl Case {
    /// Each state that blocks comes up about a quarter to a third of the
    /// time, so that some cases are mergeable.
    fn draw(r: &mut Rng) -> Self {
        Self {
            required_approvals: i32::from(r.bool()),
            require_up_to_date: r.bool(),
            require_resolved_threads: r.bool(),
            require_ci: r.bool(),
            draft: r.pick(4) == 0,
            verdict: [
                Some("approve"),
                Some("approve"),
                None,
                Some("request_changes"),
            ][r.pick(4) as usize],
            status: [Some("success"), Some("success"), None, Some("failure")][r.pick(4) as usize],
            behind: r.pick(3) == 0,
            unresolved_thread: r.pick(3) == 0,
        }
    }

    /// The requirements that can't be met, by the rules of the spec.
    fn unmet(&self) -> BTreeSet<&'static str> {
        let mut out = BTreeSet::new();
        if self.draft {
            out.insert("draft");
        }
        let approvals = i32::from(self.verdict == Some("approve"));
        if approvals < self.required_approvals {
            out.insert("approvals");
        }
        if self.verdict == Some("request_changes") {
            out.insert("changes_requested");
        }
        if self.require_up_to_date && self.behind {
            out.insert("up_to_date");
        }
        if self.require_ci && self.status != Some("success") {
            out.insert("statuses");
        }
        if self.require_resolved_threads && self.unresolved_thread {
            out.insert("threads");
        }
        out
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[test]
fn conf_review_10_mergeability_api_merge_box_and_merge_agree() {
    let seed: u64 = std::env::var("SLATH_TEST_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5eed_1a7e_c0de);
    let mut rng = Rng(seed);
    let s = TestServer::start();
    for u in ["bob", "carol"] {
        s.create_user(u, PASSWORD, false);
    }
    let alice = token(&s, "alice");
    let (bob, carol) = (token(&s, "bob"), token(&s, "carol"));
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    let url = s.url(&DEMO);
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    commit(&git, &w, "README");
    git.ok(&w, &["push", "-q", &url, "main"]);
    for who in ["bob", "carol"] {
        let r = api(
            &s,
            &alice,
            "PUT",
            &format!("/alice/demo/collaborators/{who}"),
            Some(json!({"role": "write"})),
        );
        assert!(r.status < 300, "{}", r.body);
    }
    let mut web = s.login("alice", PASSWORD);
    let (mut mergeable_cases, mut blocked_cases) = (0, 0);
    for i in 0..24 {
        let case = Case::draw(&mut rng);
        let ctx = format!("seed {seed:#x}, case {i}: {case:?}");
        let (target, source) = (format!("t{i}"), format!("s{i}"));
        let mut contexts = Vec::new();
        if case.require_ci {
            contexts.push("ci");
        }
        let r = api(
            &s,
            &alice,
            "POST",
            "/alice/demo/protections",
            Some(json!({
                "pattern": target,
                "required_approvals": case.required_approvals,
                "require_up_to_date": case.require_up_to_date,
                "require_resolved_threads": case.require_resolved_threads,
                "required_status_contexts": contexts,
            })),
        );
        assert!(r.status < 300, "{ctx}: {}", r.body);
        git.ok(&w, &["switch", "-q", "-c", &target, "main"]);
        git.ok(&w, &["push", "-q", &url, &target]);
        git.ok(&w, &["switch", "-q", "-c", &source, &target]);
        commit(&git, &w, &format!("{source}.txt"));
        git.ok(&w, &["push", "-q", &url, &source]);
        let r = api(
            &s,
            &alice,
            "POST",
            "/alice/demo/merge-requests",
            Some(
                json!({"source_branch": source, "target_branch": target, "title": source, "draft": case.draft}),
            ),
        );
        assert_eq!(r.status, 201, "{ctx}: {}", r.body);
        let n = r.json()["number"].as_i64().unwrap();
        let path = format!("/alice/demo/merge-requests/{n}");
        eventually(&s, "no first revision", || {
            api(&s, &alice, "GET", &path, None).json()["revisions"] == 1
        });
        let head = api(&s, &alice, "GET", &path, None).json()["head_oid"]
            .as_str()
            .unwrap()
            .to_owned();
        if let Some(v) = case.verdict {
            let r = api(
                &s,
                &bob,
                "POST",
                &format!("{path}/reviews"),
                Some(json!({"verdict": v})),
            );
            assert!(r.status < 300, "{ctx}: {}", r.body);
        }
        if let Some(st) = case.status {
            let r = api(
                &s,
                &carol,
                "POST",
                &format!("/alice/demo/statuses/{head}"),
                Some(json!({"state": st, "context": "ci"})),
            );
            assert!(r.status < 300, "{ctx}: {}", r.body);
        }
        let r = api(
            &s,
            &bob,
            "POST",
            &format!("{path}/threads"),
            Some(json!({"body": "A question"})),
        );
        assert!(r.status < 300, "{ctx}: {}", r.body);
        if !case.unresolved_thread {
            let id = r.json()["id"].as_str().unwrap().to_owned();
            let r = api(
                &s,
                &bob,
                "PATCH",
                &format!("{path}/threads/{id}"),
                Some(json!({"resolved": true})),
            );
            assert!(r.status < 300, "{ctx}: {}", r.body);
        }
        if case.behind {
            git.ok(&w, &["switch", "-q", &target]);
            commit(&git, &w, &format!("{target}-later.txt"));
            git.ok(&w, &["push", "-q", &url, &target]);
        }

        // 1. The API lists exactly the requirements the rules leave unmet.
        let mb: Value = api(&s, &alice, "GET", &format!("{path}/mergeability"), None).json();
        let reqs = mb["requirements"].as_array().unwrap();
        let unmet: BTreeSet<&str> = reqs
            .iter()
            .filter(|r| r["state"] != "met")
            .map(|r| r["key"].as_str().unwrap())
            .collect();
        assert_eq!(unmet, case.unmet(), "{ctx}\n{mb}");
        let mergeable = mb["mergeable"].as_bool().unwrap();
        assert_eq!(mergeable, case.unmet().is_empty(), "{ctx}\n{mb}");
        assert_eq!(mb["up_to_date"], !case.behind, "{ctx}\n{mb}");
        for r in reqs.iter().filter(|r| r["state"] != "met") {
            assert!(r["fix_action"].is_string(), "{ctx}: no fix for {r}");
        }

        // 2. The merge box shows every reason and offers merging exactly then.
        let page = web.get(&path).body;
        for r in reqs {
            let reason = html_escape(r["reason"].as_str().unwrap());
            assert!(
                page.contains(&reason),
                "{ctx}: the merge box lacks {reason:?}"
            );
        }
        assert_eq!(page.contains("id=\"merge-form\""), mergeable, "{ctx}");

        // 3. The merge decision is the same.
        let r = api(
            &s,
            &alice,
            "POST",
            &format!("{path}/merge"),
            Some(json!({"strategy": "merge-commit", "expected_head_oid": head})),
        );
        if mergeable {
            assert_eq!(r.status, 200, "{ctx}: {}", r.body);
            mergeable_cases += 1;
        } else {
            assert_eq!(r.status, 409, "{ctx}: {}", r.body);
            blocked_cases += 1;
        }
    }
    // The seed covers both outcomes.
    assert!(
        mergeable_cases > 0 && blocked_cases > 0,
        "{mergeable_cases}/{blocked_cases}"
    );
}
