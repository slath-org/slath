// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Three people on three machines work on one stack through Slath, with
//! nothing set up on any client: bob with jj through his fork, alice with
//! jj in her own repository, and carol with plain Git. Bob builds a stack
//! of four changes, carol reviews two of them, the target moves, bob
//! rebases, inserts, and reorders, carol adds a commit with Git through
//! edits by maintainers, alice lands the bottom of the stack in Slath and
//! the rest by pushing `main` with jj. Every step shows the right order,
//! base, per-change pages, outcomes, and states, and every client sees
//! the change IDs Slath shows. Runs for each pinned jj version.

use std::path::Path;

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

use crate::change_identity::{entries, ids};
use crate::fork_merge_requests::{api, eventually, fork, http_url, token};
use crate::jj::{Jj, jj_binaries};

struct World {
    s: TestServer,
    alice: String,
    bob: String,
    carol: String,
    git: GitClient,
    d: tempfile::TempDir,
}

impl World {
    /// Public alice/demo with `main`, carol a writer, and bob's fork.
    fn start() -> Self {
        let s = TestServer::start();
        for u in ["bob", "carol"] {
            s.create_user(u, PASSWORD, false);
        }
        let (alice, bob, carol) = (token(&s, "alice"), token(&s, "bob"), token(&s, "carol"));
        let w = Self {
            s,
            alice,
            bob,
            carol,
            git: GitClient::new(),
            d: tempfile::tempdir().unwrap(),
        };
        let seed = w.d.path().join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        w.git.ok(&seed, &["init", "-q", "-b", "main"]);
        write(&seed, "README", "demo\n");
        w.git.ok(&seed, &["add", "."]);
        w.git.ok(&seed, &["commit", "-q", "-m", "init"]);
        w.git.ok(&seed, &["push", "-q", &w.s.url(&DEMO), "main"]);
        for (method, path, body) in [
            ("PATCH", "/alice/demo", json!({"visibility": "public"})),
            (
                "PUT",
                "/alice/demo/collaborators/carol",
                json!({"role": "write"}),
            ),
        ] {
            let r = api(&w.s, &w.alice, method, path, Some(body));
            assert!(r.status < 300, "{}", r.body);
        }
        fork(&w.s, &w.bob, json!({"namespace": "bob"}), "/bob/demo/fork");
        w
    }

    fn mr(&self, n: i64) -> Value {
        api(
            &self.s,
            &self.alice,
            "GET",
            &format!("/alice/demo/merge-requests/{n}"),
            None,
        )
        .json()
    }

    /// The stack once merge request `n` has `revisions` revisions.
    fn stack(&self, n: i64, revisions: i64) -> Value {
        eventually(&self.s, &format!("!{n}: no revision {revisions}"), || {
            self.mr(n)["revisions"] == revisions
        });
        let r = api(
            &self.s,
            &self.alice,
            "GET",
            &format!("/alice/demo/merge-requests/{n}/stack"),
            None,
        );
        assert_eq!(r.status, 200, "{}", r.body);
        r.json()
    }

    /// The change page of `change` in merge request `n`, as alice sees it.
    fn change_page(&self, n: i64, change: &str) -> String {
        let mut ui = self.s.login("alice", PASSWORD);
        let r = ui.get(&format!("/alice/demo/merge-requests/{n}/changes/{change}"));
        assert_eq!(r.status, 200, "{change}: {}", r.body);
        r.body
    }

    /// The change IDs of the per-change reviews that count.
    fn counting_reviews(&self, n: i64) -> Vec<String> {
        let r = api(
            &self.s,
            &self.alice,
            "GET",
            &format!("/alice/demo/merge-requests/{n}/reviews"),
            None,
        )
        .json();
        r.as_array()
            .unwrap()
            .iter()
            .filter(|r| r["counts"] == true)
            .filter_map(|r| r["change_id"].as_str().map(str::to_owned))
            .collect()
    }

    /// The head of alice/demo's `main` on the server.
    fn main_head(&self) -> String {
        self.git
            .ok(&self.s.repo_path(&DEMO), &["rev-parse", "main"])
            .trim()
            .to_owned()
    }
}

fn write(dir: &Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
}

/// The change IDs of `revset`, newest first, one per line.
fn change_ids(jj: &Jj, repo: &Path, revset: &str) -> String {
    jj.ok(
        repo,
        &[
            "log",
            "--no-graph",
            "-r",
            revset,
            "-T",
            "change_id ++ \"\\n\"",
        ],
    )
}

#[test]
fn conf_jj_04_two_jj_machines_and_a_git_clone_through_a_fork() {
    let w = World::start();
    let d = w.d.path();
    let alice_url = w.s.url(&DEMO);
    for (version, bin) in jj_binaries() {
        let v = version.replace(['.', ' '], "-");
        let topic = format!("stack-{v}");

        // Bob, with jj, clones his fork and follows alice's repository.
        let bob = Jj::new(&version, &bin);
        let b = d.join(format!("bob-{v}"));
        bob.ok(
            d,
            &[
                "git",
                "clone",
                &http_url(&w.s, &w.bob, "bob/demo"),
                b.to_str().unwrap(),
            ],
        );
        bob.ok(
            &b,
            &[
                "git",
                "remote",
                "add",
                "upstream",
                &http_url(&w.s, &w.bob, "alice/demo"),
            ],
        );
        bob.ok(&b, &["git", "fetch", "--remote", "upstream"]);
        // A stack of four changes on alice's main.
        let mut made = Vec::new();
        for (i, x) in ["a", "b", "c", "d"].iter().enumerate() {
            let parent = if i == 0 { "main@upstream" } else { "@" };
            bob.ok(&b, &["new", parent, "-m", &format!("Add {x}")]);
            write(&b, &format!("{v}-{x}.txt"), &format!("{x}\n"));
            made.push(bob.show(&b, "@", "change_id"));
        }
        let [ca, cb, cc, cd] = [0, 1, 2, 3].map(|i| made[i].clone());
        bob.ok(&b, &["bookmark", "create", &topic, "-r", "@"]);
        bob.ok(&b, &["new", "main@upstream"]);
        bob.ok(&b, &["git", "push", "--bookmark", &topic]);
        let r = api(
            &w.s,
            &w.bob,
            "POST",
            "/alice/demo/merge-requests",
            Some(json!({"source_repo": "bob/demo", "source_branch": topic,
                        "title": format!("Stack {v}"), "allow_maintainer_edits": true})),
        );
        assert_eq!(r.status, 201, "{version}: {}", r.body);
        let n = r.json()["number"].as_i64().unwrap();
        let s = w.stack(n, 1);
        assert_eq!(ids(&s), made, "{version}: {s}");
        let base = w.main_head();
        let revs = api(
            &w.s,
            &w.alice,
            "GET",
            &format!("/alice/demo/merge-requests/{n}/revisions"),
            None,
        )
        .json();
        assert_eq!(revs[0]["base_oid"], base.as_str(), "{version}: {revs}");

        // Carol, with Git, reviews the two bottom changes one by one and
        // moves alice's main meanwhile.
        for change in [&ca, &cb] {
            let r = api(
                &w.s,
                &w.carol,
                "POST",
                &format!("/alice/demo/merge-requests/{n}/reviews"),
                Some(json!({"verdict": "approve", "change": change})),
            );
            assert!(r.status < 300, "{version}: {}", r.body);
        }
        let c = d.join(format!("carol-{v}"));
        let carol_main = http_url(&w.s, &w.carol, "alice/demo");
        w.git
            .ok(d, &["clone", "-q", &carol_main, c.to_str().unwrap()]);
        write(&c, &format!("{v}-main.txt"), "main\n");
        w.git.ok(&c, &["add", "."]);
        w.git
            .ok(&c, &["commit", "-q", "-m", &format!("Main moves {v}")]);
        w.git.ok(&c, &["push", "-q", "origin", "main"]);

        // Bob rebases onto the new main, inserts a change after b, and
        // moves d below c.
        bob.ok(&b, &["git", "fetch", "--remote", "upstream"]);
        bob.ok(&b, &["rebase", "-b", &topic, "-d", "main@upstream"]);
        bob.ok(&b, &["new", "--insert-after", &cb, "-m", "Add b2"]);
        write(&b, &format!("{v}-b2.txt"), "b2\n");
        let cb2 = bob.show(&b, "@", "change_id");
        bob.ok(&b, &["rebase", "-r", &cd, "--insert-before", &cc]);
        bob.ok(&b, &["bookmark", "set", &topic, "-r", &cc]);
        bob.ok(&b, &["new", "main@upstream"]);
        bob.ok(&b, &["git", "push", "--bookmark", &topic]);
        let s = w.stack(n, 2);
        let order = vec![ca.clone(), cb.clone(), cb2.clone(), cd.clone(), cc.clone()];
        assert_eq!(ids(&s), order, "{version}: {s}");
        let outcomes: Vec<&str> = entries(&s)
            .iter()
            .map(|e| e["outcome"].as_str().unwrap())
            .collect();
        assert_eq!(
            outcomes,
            ["unchanged", "unchanged", "new", "unchanged", "unchanged"],
            "{version}: {s}"
        );
        let revs = api(
            &w.s,
            &w.alice,
            "GET",
            &format!("/alice/demo/merge-requests/{n}/revisions"),
            None,
        )
        .json();
        assert_eq!(
            revs[1]["base_oid"],
            w.main_head().as_str(),
            "{version}: {revs}"
        );
        assert_ne!(revs[1]["base_oid"], base.as_str(), "{version}");
        // The rebase kept carol's approvals: the patches are the same.
        assert_eq!(
            w.counting_reviews(n),
            vec![ca.clone(), cb.clone()],
            "{version}"
        );
        // The inserted change's page shows its own file in the new order.
        let page = w.change_page(n, &cb2);
        assert!(page.contains(&format!("{v}-b2.txt")), "{version}");
        assert!(page.contains(">Change 3 of 5<"), "{version}");
        assert!(page.contains("New in revision 2"), "{version}");

        // Carol adds a commit with Git through edits by maintainers; the
        // stack is now mixed, and bob's jj picks it up.
        let cf = d.join(format!("carol-fork-{v}"));
        let carol_fork = http_url(&w.s, &w.carol, "bob/demo");
        w.git.ok(
            d,
            &[
                "clone",
                "-q",
                "-b",
                &topic,
                &carol_fork,
                cf.to_str().unwrap(),
            ],
        );
        write(&cf, &format!("{v}-e.txt"), "e\n");
        w.git.ok(&cf, &["add", "."]);
        w.git.ok(&cf, &["commit", "-q", "-m", "Add e"]);
        let e_commit = w.git.ok(&cf, &["rev-parse", "HEAD"]).trim().to_owned();
        w.git.ok(&cf, &["push", "-q", "origin", &topic]);
        let s = w.stack(n, 3);
        let ce = ids(&s)[5].clone();
        assert_eq!(&ids(&s)[..5], &order[..], "{version}: {s}");
        assert_eq!(entries(&s)[5]["commit_oid"], e_commit.as_str(), "{version}");
        bob.ok(&b, &["git", "fetch"]);
        assert_eq!(
            bob.show(&b, &format!("{topic}@origin"), "change_id"),
            ce,
            "{version}: jj shows the ID Slath shows for a Git commit"
        );

        // Alice lands the stack up to b in Slath; the rest is rebased on
        // the fork's branch and keeps its IDs.
        let r = api(
            &w.s,
            &w.alice,
            "POST",
            &format!("/alice/demo/merge-requests/{n}/merge"),
            Some(json!({"strategy": "rebase", "expected_head_oid": e_commit,
                        "upto_change": cb})),
        );
        assert_eq!(r.status, 200, "{version}: {}", r.body);
        let s = w.stack(n, 4);
        let rest = vec![cb2.clone(), cd.clone(), cc.clone(), ce.clone()];
        assert_eq!(ids(&s), rest, "{version}: {s}");
        assert_eq!(w.mr(n)["state"], "open", "{version}");
        bob.ok(&b, &["git", "fetch", "--remote", "upstream"]);
        let on_main = change_ids(&bob, &b, "::main@upstream");
        for id in [&ca, &cb] {
            assert!(on_main.contains(id.as_str()), "{version}: {on_main}");
        }
        bob.ok(&b, &["git", "fetch"]);
        assert_eq!(
            bob.show(&b, &format!("{topic}@origin-"), "change_id"),
            cc,
            "{version}"
        );
        assert_eq!(
            bob.show(&b, &format!("{topic}@origin"), "change_id"),
            ce,
            "{version}"
        );

        // Alice, with jj, lands the rest by pushing main.
        let alice = Jj::new(&version, &bin);
        let a = d.join(format!("alice-{v}"));
        alice.ok(d, &["git", "clone", &alice_url, a.to_str().unwrap()]);
        alice.ok(
            &a,
            &[
                "git",
                "remote",
                "add",
                "bob",
                &http_url(&w.s, &w.alice, "bob/demo"),
            ],
        );
        alice.ok(&a, &["git", "fetch", "--remote", "bob"]);
        alice.ok(
            &a,
            &["bookmark", "set", "main", "-r", &format!("{topic}@bob")],
        );
        alice.ok(
            &a,
            &["git", "push", "--remote", "origin", "--bookmark", "main"],
        );
        eventually(&w.s, &format!("{version}: !{n} not merged"), || {
            w.mr(n)["state"] == "merged"
        });
        let s = api(
            &w.s,
            &w.alice,
            "GET",
            &format!("/alice/demo/merge-requests/{n}/stack"),
            None,
        )
        .json();
        assert!(
            entries(&s).iter().all(|e| e["change_state"] == "merged"),
            "{version}: {s}"
        );

        // Everyone sees the whole stack on main, with the same IDs.
        let on_main = change_ids(&alice, &a, "::main@origin");
        for id in [&ca, &cb, &cb2, &cd, &cc, &ce] {
            assert!(on_main.contains(id.as_str()), "{version}: {id}: {on_main}");
        }
        w.git.ok(&c, &["pull", "-q", "--ff-only"]);
        let files = w.git.ok(&c, &["ls-files"]);
        for x in ["a", "b", "b2", "c", "d", "e", "main"] {
            assert!(files.contains(&format!("{v}-{x}.txt")), "{version}: {x}");
        }
    }
}
