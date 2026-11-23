// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Linked merge requests (AGENT-07): merge requests in several
//! repositories grouped to merge together. Linking needs the edit right on
//! every member, people see only the members they can read, and a linked
//! merge request merges only with its group.

use std::path::Path;

use serde_json::{Value, json};
use slath_tests::server::{GitClient, PASSWORD, TestRepo, TestServer};

const LIB: TestRepo = TestRepo {
    namespace: "alice",
    name: "lib",
};
const APP: TestRepo = TestRepo {
    namespace: "alice",
    name: "app",
};

pub(crate) struct Ctx {
    pub s: TestServer,
    pub alice: String,
    pub bob: String,
    pub git: GitClient,
    pub dir: tempfile::TempDir,
}

fn token(s: &TestServer, user: &str) -> String {
    let mut c = s.login(user, PASSWORD);
    slath_tests::http::create_token(&mut c, "g", &["repo:write", "mrs:write"])
}

impl Ctx {
    /// `alice/lib` and `alice/app`, each with `main` and one open merge
    /// request (!1) from `topic`; `bob` reads `alice/app` only.
    pub fn start() -> Self {
        let s = TestServer::start_with(&[LIB, APP]);
        s.create_user("bob", PASSWORD, false);
        let c = Self {
            alice: token(&s, "alice"),
            bob: token(&s, "bob"),
            s,
            git: GitClient::new(),
            dir: tempfile::tempdir().unwrap(),
        };
        let r = c.api(
            &c.alice.clone(),
            "PUT",
            "alice/app/collaborators/bob",
            Some(json!({"role": "read"})),
        );
        assert!(r.status < 300, "{}", r.body);
        for repo in [&LIB, &APP] {
            c.seed(repo);
        }
        c
    }

    fn seed(&self, repo: &TestRepo) {
        let w = self.dir.path().join(repo.name);
        std::fs::create_dir_all(&w).unwrap();
        let url = self.s.url(repo);
        self.git.ok(&w, &["init", "-q", "-b", "main"]);
        commit(&self.git, &w, "README", "init");
        self.git.ok(&w, &["push", "-q", &url, "main"]);
        self.git.ok(&w, &["switch", "-q", "-c", "topic"]);
        commit(&self.git, &w, &format!("{}.txt", repo.name), "Add it");
        self.git.ok(&w, &["push", "-q", &url, "topic"]);
        let r = self.api(
            &self.alice,
            "POST",
            &format!("{}/{}/merge-requests", repo.namespace, repo.name),
            Some(json!({"source_branch": "topic", "title": format!("Change {}", repo.name)})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        self.head(&format!("{}/{}", repo.namespace, repo.name), 1);
    }

    pub fn api(
        &self,
        token: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> slath_tests::http::Resp {
        self.s.client().api(
            method,
            &format!("/api/v1/repos/{path}"),
            Some(token),
            body.as_ref(),
        )
    }

    /// The head of `repo`'s !n once it has its first revision.
    pub fn head(&self, repo: &str, n: i64) -> String {
        let start = std::time::Instant::now();
        loop {
            let m = self
                .api(
                    &self.alice,
                    "GET",
                    &format!("{repo}/merge-requests/{n}"),
                    None,
                )
                .json();
            if let Some(h) = m["head_oid"].as_str() {
                return h.to_owned();
            }
            assert!(
                start.elapsed().as_secs() < 30,
                "no revision\n{}",
                self.s.log()
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    pub fn state(&self, repo: &str, n: i64) -> String {
        self.api(
            &self.alice,
            "GET",
            &format!("{repo}/merge-requests/{n}"),
            None,
        )
        .json()["state"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// Links `alice/lib!1` and `alice/app!1`, in this order.
    pub fn link(&self) -> Value {
        let r = self.api(
            &self.alice,
            "PUT",
            "alice/lib/merge-requests/1/group",
            Some(json!({"members": ["alice/lib!1", "alice/app!1"]})),
        );
        assert_eq!(r.status, 200, "{}", r.body);
        r.json()
    }
}

fn commit(git: &GitClient, w: &Path, file: &str, msg: &str) {
    std::fs::write(w.join(file), format!("{msg}\n")).unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", msg]);
}

fn refs(g: &Value) -> Vec<String> {
    g["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["merge_request"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn conf_agent_07_members_are_linked_in_order_by_people_who_may_edit_them() {
    let c = Ctx::start();
    let g = c.link();
    assert_eq!(refs(&g), ["alice/lib!1", "alice/app!1"]);
    assert_eq!(g["state"], "open");
    assert_eq!(g["can_edit"], true);
    assert_eq!(g["members"][0]["untrusted_content"], true);
    // The same group from the other member, and reordered.
    let r = c.api(
        &c.alice,
        "PUT",
        "alice/app/merge-requests/1/group",
        Some(json!({"members": ["http://x.example/alice/app/merge-requests/1", "alice/lib#1"]})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let g = c
        .api(&c.alice, "GET", "alice/lib/merge-requests/1/group", None)
        .json();
    assert_eq!(refs(&g), ["alice/app!1", "alice/lib!1"]);
    // Malformed lists.
    for (members, code) in [
        (json!(["alice/lib!1"]), "count"),
        (json!(["alice/lib!1", "alice/lib!1"]), "duplicate"),
        (json!(["alice/app!1", "nonsense"]), "reference"),
        (json!(["alice/app!1", "alice/app!1"]), "duplicate"),
    ] {
        let r = c.api(
            &c.alice,
            "PUT",
            "alice/lib/merge-requests/1/group",
            Some(json!({ "members": members })),
        );
        assert_eq!(r.status, 422, "{members}: {}", r.body);
        assert!(r.body.contains(code), "{members}: {}", r.body);
    }
    let r = c.api(
        &c.alice,
        "PUT",
        "alice/lib/merge-requests/1/group",
        Some(json!({"members": ["alice/app!1", "alice/app!2"]})),
    );
    assert_eq!(r.status, 404, "an unknown member: {}", r.body);
    // Unlinking one of two dissolves the group.
    let r = c.api(&c.alice, "DELETE", "alice/app/merge-requests/1/group", None);
    assert_eq!(r.status, 204, "{}", r.body);
    for repo in ["alice/app", "alice/lib"] {
        let r = c.api(
            &c.alice,
            "GET",
            &format!("{repo}/merge-requests/1/group"),
            None,
        );
        assert_eq!(r.status, 404, "{repo}: {}", r.body);
    }
}

#[test]
fn conf_agent_07_people_see_only_the_members_they_can_read() {
    let c = Ctx::start();
    c.link();
    // bob reads alice/app only: its group has one member for him.
    let g = c.api(&c.bob, "GET", "alice/app/merge-requests/1/group", None);
    assert_eq!(g.status, 200, "{}", g.body);
    let g = g.json();
    assert_eq!(refs(&g), ["alice/app!1"]);
    assert_eq!(g["can_edit"], false);
    assert_eq!(g["can_merge"], false);
    assert!(!g.to_string().contains("lib"), "{g}");
    // The other member and its group are not found for him.
    let r = c.api(&c.bob, "GET", "alice/lib/merge-requests/1/group", None);
    assert_eq!(r.status, 404, "{}", r.body);
    // Reading isn't editing.
    let r = c.api(
        &c.bob,
        "PUT",
        "alice/app/merge-requests/1/group",
        Some(json!({"members": ["alice/app!1", "alice/lib!1"]})),
    );
    assert_eq!(r.status, 403, "{}", r.body);
    let r = c.api(&c.bob, "DELETE", "alice/app/merge-requests/1/group", None);
    assert_eq!(r.status, 403, "{}", r.body);
}

#[test]
fn conf_agent_07_linked_merge_requests_merge_only_with_their_group() {
    let c = Ctx::start();
    // Merge when ready keeps a merge request out of groups.
    let r = c.api(
        &c.alice,
        "POST",
        "alice/lib/protections",
        Some(json!({"pattern": "main", "required_approvals": 1})),
    );
    assert!(r.status < 300, "{}", r.body);
    let r = c.api(
        &c.alice,
        "PUT",
        "alice/lib/merge-requests/1/auto-merge",
        Some(json!({})),
    );
    assert!(r.status < 300, "{}", r.body);
    let members = json!({"members": ["alice/lib!1", "alice/app!1"]});
    let r = c.api(
        &c.alice,
        "PUT",
        "alice/app/merge-requests/1/group",
        Some(members.clone()),
    );
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(r.body.contains("Cancel merge when ready"), "{}", r.body);
    let r = c.api(
        &c.alice,
        "DELETE",
        "alice/lib/merge-requests/1/auto-merge",
        None,
    );
    assert_eq!(r.status, 204, "{}", r.body);
    c.link();
    // Linked, neither merges nor merges when ready alone.
    let head = c.head("alice/app", 1);
    let merge = json!({"expected_head_oid": head});
    let r = c.api(
        &c.alice,
        "POST",
        "alice/app/merge-requests/1/merge",
        Some(merge.clone()),
    );
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(r.body.contains("merge the group"), "{}", r.body);
    let r = c.api(
        &c.alice,
        "PUT",
        "alice/app/merge-requests/1/auto-merge",
        Some(json!({})),
    );
    assert_eq!(r.status, 409, "{}", r.body);
    assert_eq!(c.state("alice/app", 1), "open");
    // Unlinked, it merges alone again.
    let r = c.api(&c.alice, "DELETE", "alice/app/merge-requests/1/group", None);
    assert_eq!(r.status, 204, "{}", r.body);
    let r = c.api(
        &c.alice,
        "POST",
        "alice/app/merge-requests/1/merge",
        Some(merge),
    );
    assert_eq!(r.status, 200, "{}", r.body);
}

impl Ctx {
    fn main_of(&self, repo: &TestRepo) -> String {
        self.git
            .ok(&self.s.repo_path(repo), &["rev-parse", "main"])
            .trim()
            .to_owned()
    }

    /// The group merge request for the open members, in this order.
    fn merge_body(&self, members: &[&str]) -> Value {
        let members: Vec<Value> = members
            .iter()
            .map(|r| {
                let (repo, n) = r.split_once('!').unwrap();
                json!({"merge_request": r, "expected_head_oid": self.head(repo, n.parse().unwrap())})
            })
            .collect();
        json!({ "members": members })
    }

    fn merge_group(&self, token: &str, members: &[&str]) -> slath_tests::http::Resp {
        let body = self.merge_body(members);
        self.api(
            token,
            "POST",
            "alice/app/merge-requests/1/group/merge",
            Some(body),
        )
    }

    fn merged_at(&self, repo: &str) -> String {
        self.api(
            &self.alice,
            "GET",
            &format!("{repo}/merge-requests/1"),
            None,
        )
        .json()["merged_at"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }
}

#[test]
fn conf_agent_07_a_group_merges_only_when_every_member_is_mergeable() {
    let c = Ctx::start();
    c.link();
    let both = ["alice/lib!1", "alice/app!1"];
    let (lib, app) = (c.main_of(&LIB), c.main_of(&APP));
    // alice/app needs an approval: nothing merges, and the refusal says why.
    let r = c.api(
        &c.alice,
        "POST",
        "alice/app/protections",
        Some(json!({"pattern": "main", "required_approvals": 1})),
    );
    assert!(r.status < 300, "{}", r.body);
    let rule = r.json()["id"].as_str().unwrap().to_owned();
    let r = c.merge_group(&c.alice, &both);
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(r.body.contains("alice/app!1"), "{}", r.body);
    assert!(
        !r.body.contains("alice/lib!1:"),
        "lib is mergeable: {}",
        r.body
    );
    assert_eq!(
        (c.main_of(&LIB), c.main_of(&APP)),
        (lib.clone(), app.clone())
    );
    assert_eq!(c.state("alice/lib", 1), "open");
    let g = c
        .api(&c.alice, "GET", "alice/lib/merge-requests/1/group", None)
        .json();
    assert_eq!(
        (g["ready"].as_bool(), g["state"].as_str()),
        (Some(false), Some("open"))
    );
    // The request must name every open member in order with its head.
    let r = c.merge_group(&c.alice, &["alice/app!1", "alice/lib!1"]);
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(r.body.contains("changed since you looked"), "{}", r.body);
    let r = c.api(
        &c.alice,
        "DELETE",
        &format!("alice/app/protections/{rule}"),
        None,
    );
    assert!(r.status < 300, "{}", r.body);
    // Someone who can't read every member can't merge it.
    let r = c.merge_group(&c.bob, &["alice/app!1"]);
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(r.body.contains("every merge request in it"), "{}", r.body);
    assert!(!r.body.contains("lib"), "{}", r.body);
    // Ready: both merge, in the group's order.
    let r = c.merge_group(&c.alice, &both);
    assert_eq!(r.status, 200, "{}", r.body);
    let o = r.json();
    assert_eq!(o["merged"], json!(both));
    assert_eq!(o["remaining"], json!([]));
    assert_eq!(o["failure"], Value::Null);
    assert_eq!(o["group"]["state"], "merged");
    assert_ne!((c.main_of(&LIB), c.main_of(&APP)), (lib, app));
    assert!(c.merged_at("alice/lib") <= c.merged_at("alice/app"));
}

#[test]
fn conf_agent_07_a_failure_after_the_first_merge_leaves_the_group_partially_landed() {
    let c = Ctx::start();
    c.link();
    let lib = c.main_of(&LIB);
    let app = c.main_of(&APP);
    // A held ref lock makes the second merge's target update fail after
    // its checks passed.
    let lock = c.s.repo_path(&APP).join("refs/heads/main.lock");
    std::fs::write(&lock, "").unwrap();
    let r = c.merge_group(&c.alice, &["alice/lib!1", "alice/app!1"]);
    std::fs::remove_file(&lock).unwrap();
    assert_eq!(r.status, 200, "{}", r.body);
    let o = r.json();
    assert_eq!(o["merged"], json!(["alice/lib!1"]));
    assert_eq!(o["remaining"], json!(["alice/app!1"]));
    assert!(o["failure"].as_str().is_some_and(|f| !f.is_empty()), "{o}");
    assert_eq!(o["group"]["state"], "partially_landed");
    assert_ne!(c.main_of(&LIB), lib);
    assert_eq!(c.main_of(&APP), app);
    assert_eq!(c.state("alice/app", 1), "open");
    // The group says where it stopped; bob sees the reason only for what
    // he can read, and no more.
    let g = c
        .api(&c.alice, "GET", "alice/lib/merge-requests/1/group", None)
        .json();
    assert_eq!(g["state"], "partially_landed");
    assert_eq!(g["last_failure"]["merge_request"], "alice/app!1");
    let g = c
        .api(&c.bob, "GET", "alice/app/merge-requests/1/group", None)
        .json();
    assert_eq!(g["last_failure"]["merge_request"], "alice/app!1");
    assert!(!g.to_string().contains("lib"), "{g}");
    // The remaining member still merges only with the group; merging the
    // group again continues with it.
    let head = c.head("alice/app", 1);
    let r = c.api(
        &c.alice,
        "POST",
        "alice/app/merge-requests/1/merge",
        Some(json!({"expected_head_oid": head})),
    );
    assert_eq!(r.status, 409, "{}", r.body);
    let r = c.merge_group(&c.alice, &["alice/app!1"]);
    assert_eq!(r.status, 200, "{}", r.body);
    let o = r.json();
    assert_eq!(o["merged"], json!(["alice/app!1"]));
    assert_eq!(o["group"]["state"], "merged");
    assert_eq!(o["group"]["last_failure"], Value::Null);
}
