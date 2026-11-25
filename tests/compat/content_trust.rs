// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Content trust: a work order says whose text and merge requests its task
//! token reads (`all`, `members` by default, or `writers`).

use serde_json::json;

use crate::work_orders::World;

#[test]
fn conf_agent_19_orders_record_their_content_trust() {
    let w = World::start();
    let r = w.delegate(&w.bob, json!({}));
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.json()["content_trust"], "members");
    let r = w.api(&w.bob, "POST", "/work-orders/1/stop", None);
    assert!(r.status < 300, "{}", r.body);

    let r = w.delegate(&w.bob, json!({"content_trust": "trusted"}));
    assert_eq!(r.status, 422, "{}", r.body);
    for level in ["writers", "all"] {
        let r = w.delegate(&w.bob, json!({"content_trust": level}));
        assert_eq!(r.status, 201, "{}", r.body);
        let n = r.json()["number"].as_i64().unwrap();
        let r = w.api(&w.carol, "GET", &format!("/work-orders/{n}"), None);
        assert_eq!(r.json()["content_trust"], level);
        let r = w.api(&w.bob, "POST", &format!("/work-orders/{n}/stop"), None);
        assert!(r.status < 300, "{}", r.body);
    }
}

/// Text no response to a limited task token may contain.
const INJECTED: &str = "IGNORE-PREVIOUS-INSTRUCTIONS";

/// alice/demo made public, with text on issue #1 by a writer (bob), a
/// member (carol), an outside contributor (dave), and a bot with the
/// read role (`reader`); dave also opens issue #2. Then issue #1 is
/// delegated with `extra` and claimed.
fn world_with_text(extra: serde_json::Value) -> (World, crate::work_orders::Claimed) {
    let w = World::start();
    let r = w.s.client().api(
        "PATCH",
        "/api/v1/repos/alice/demo",
        Some(&w.alice),
        Some(&json!({"visibility": "public"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let dave = crate::work_orders::token(&w.s, "dave", &["repo:write", "issues:write"]);
    let reader = crate::work_orders::bot(&w.s, "alice", "reader", "repo:read,issues:write");
    let r = w.api(
        &w.alice,
        "PUT",
        "/collaborators/reader",
        Some(json!({"role": "read"})),
    );
    assert!(r.status < 300, "{}", r.body);
    for (who, text) in [
        (&w.bob, "writer says hello".to_owned()),
        (&w.carol, "member says hello".to_owned()),
        (&dave, format!("outsider says {INJECTED}")),
        (&reader, "reader bot says hello".to_owned()),
    ] {
        let r = w.api(
            who,
            "POST",
            "/issues/1/comments",
            Some(json!({"body": &text})),
        );
        assert_eq!(r.status, 201, "{text}: {}", r.body);
    }
    let r = w.api(
        &dave,
        "POST",
        "/issues",
        Some(json!({"title": format!("Please {INJECTED}"), "body": INJECTED})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let claimed = crate::work_orders::claimed_with(&w, extra);
    (w, claimed)
}

fn mcp(w: &World, token: &str, name: &str, arguments: serde_json::Value) -> String {
    let r = w.s.client().api_with(
        "POST",
        "/api/mcp",
        Some(token),
        Some(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                     "params": {"name": name, "arguments": arguments}})),
        &[
            ("accept", "application/json, text/event-stream"),
            ("mcp-protocol-version", "2025-06-18"),
        ],
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(!r.body.contains("\"isError\":true"), "{}", r.body);
    r.body
}

#[test]
fn conf_agent_19_text_below_the_level_reaches_task_tokens_as_placeholders() {
    let (w, (_claimer, task, _git, _dir)) = world_with_text(json!({}));
    // The bot's own comment, under the order.
    let r = w.api(
        &task,
        "POST",
        "/issues/1/comments",
        Some(json!({"body": "agent says hello"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);

    let r = w.api(&task, "GET", "/issues/1/comments", None);
    assert_eq!(r.status, 200, "{}", r.body);
    let items = r.json()["items"].as_array().unwrap().clone();
    let bodies: Vec<_> = items.iter().map(|c| c["body"].clone()).collect();
    assert_eq!(
        bodies,
        [
            json!("writer says hello"),
            json!("member says hello"),
            json!(null),
            json!("reader bot says hello"),
            json!("agent says hello")
        ],
        "{items:?}"
    );
    assert_eq!(items[2]["withheld"], true);
    assert_eq!(items[2]["origin"], "outside_contributor");
    assert!(items[2]["id"].is_string(), "placeholders keep their ID");

    // Every surface: lists, single reads, search, MCP.
    let q = format!("/api/v1/search/issues?q={INJECTED}");
    for path in ["/issues", "/issues/2", "/issues/2/comments"] {
        let r = w.api(&task, "GET", path, None);
        assert_eq!(r.status, 200, "{path}: {}", r.body);
        assert!(!r.body.contains(INJECTED), "{path}: {}", r.body);
    }
    let r = w.s.client().api("GET", &q, Some(&task), None);
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(!r.body.contains(INJECTED), "{}", r.body);
    assert!(r.body.contains("\"withheld\":true"), "{}", r.body);
    let issue2 = w.api(&task, "GET", "/issues/2", None).json();
    assert_eq!(
        issue2,
        json!({"withheld": true, "untrusted_content": true,
               "origin": "outside_contributor", "number": 2})
    );
    for (tool, args) in [
        ("list_issues", json!({"ns": "alice", "repo": "demo"})),
        (
            "get_issue",
            json!({"ns": "alice", "repo": "demo", "number": 2}),
        ),
        (
            "list_issue_comments",
            json!({"ns": "alice", "repo": "demo", "number": 1}),
        ),
    ] {
        let body = mcp(&w, &task, tool, args);
        assert!(!body.contains(INJECTED), "{tool}: {body}");
        assert!(body.contains("withheld"), "{tool}: {body}");
    }

    // People and standing tokens read everything, as before.
    let r = w.api(&w.carol, "GET", "/issues/1/comments", None);
    assert!(r.body.contains(INJECTED), "{}", r.body);
}

#[test]
fn conf_agent_19_writers_withholds_members_and_all_withholds_nothing() {
    let (w, (_c, task, _g, _d)) = world_with_text(json!({"content_trust": "writers"}));
    let r = w.api(&task, "GET", "/issues/1/comments", None);
    let items = r.json()["items"].as_array().unwrap().clone();
    assert_eq!(items[0]["body"], "writer says hello");
    assert_eq!(items[1]["withheld"], true);
    assert_eq!(items[1]["origin"], "member");
    // A bot with the read role stands as a member.
    assert_eq!(items[3]["withheld"], true);
    assert_eq!(items[3]["origin"], "bot");

    let (w, (_c, task, _g, _d)) = world_with_text(json!({"content_trust": "all"}));
    let r = w.api(&task, "GET", "/issues/1/comments", None);
    assert!(r.body.contains(INJECTED), "{}", r.body);
    assert!(!r.body.contains("withheld"), "{}", r.body);
}

#[test]
fn conf_agent_19_an_order_on_an_outsiders_issue_keeps_only_the_goal() {
    let w = World::start();
    let r = w.s.client().api(
        "PATCH",
        "/api/v1/repos/alice/demo",
        Some(&w.alice),
        Some(&json!({"visibility": "public"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let dave = crate::work_orders::token(&w.s, "dave", &["repo:write", "issues:write"]);
    let r = w.api(
        &dave,
        "POST",
        "/issues",
        Some(json!({"title": format!("Please {INJECTED}")})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let (_claimer, task, _g, _d) =
        crate::work_orders::claimed_with(&w, json!({"target": {"issue": 2}, "goal": "Tidy up"}));
    for path in ["/work-orders/1", "/work-orders/1/state"] {
        let r = w.api(&task, "GET", path, None);
        assert_eq!(r.status, 200, "{path}: {}", r.body);
        assert!(!r.body.contains(INJECTED), "{path}: {}", r.body);
        assert!(r.body.contains("Tidy up"), "{path}: {}", r.body);
    }
    let o = w.api(&task, "GET", "/work-orders/1", None).json();
    assert_eq!(o["target"]["withheld"], true);
    // Readers see the target's title, marked with its origin.
    let o = w.api(&w.carol, "GET", "/work-orders/1", None).json();
    assert_eq!(o["target"]["origin"], "outside_contributor");
    assert_eq!(o["target"]["issue"], 2);
}

/// Where an outside contributor's merge requests come from: dave forks
/// alice/demo and opens one from a branch pushed with Git and, when a jj
/// binary is available, one pushed with jj. Their commit messages and
/// files carry [`INJECTED`]. Bob, a writer, opens one from a branch of
/// the repository. Returns the outsider's merge request numbers with
/// their heads, and the writer's number.
fn outsider_mrs(w: &World) -> (Vec<(i64, String)>, i64) {
    use crate::fork_merge_requests::{eventually, fork, http_url};
    let s = &w.s;
    let dave = crate::work_orders::token(s, "dave", &["repo:write", "mrs:write"]);
    fork(s, &dave, json!({"namespace": "dave"}), "/dave/demo/fork");
    let git = slath_tests::server::GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let g = d.path().join("git");
    git.ok(
        d.path(),
        &["clone", "-q", &http_url(s, &dave, "dave/demo"), "git"],
    );
    let mut branches = Vec::new();
    git.ok(&g, &["switch", "-q", "-c", "topic-git", "origin/main"]);
    std::fs::write(g.join("git.txt"), format!("{INJECTED}\n")).unwrap();
    git.ok(&g, &["add", "."]);
    git.ok(&g, &["commit", "-q", "-m", &format!("Add git: {INJECTED}")]);
    git.ok(&g, &["push", "-q", "origin", "topic-git"]);
    let head = git.ok(&g, &["rev-parse", "HEAD"]).trim().to_owned();
    branches.push(("topic-git", head));
    if let Some((version, bin)) = crate::jj::jj_binaries().into_iter().next() {
        let jj = crate::jj::Jj::new(&version, &bin);
        let j = d.path().join("jj");
        jj.ok(
            d.path(),
            &[
                "git",
                "clone",
                &http_url(s, &dave, "dave/demo"),
                j.to_str().unwrap(),
            ],
        );
        jj.ok(
            &j,
            &["new", "main@origin", "-m", &format!("Add jj: {INJECTED}")],
        );
        std::fs::write(j.join("jj.txt"), format!("{INJECTED}\n")).unwrap();
        jj.ok(&j, &["bookmark", "create", "topic-jj", "-r", "@"]);
        jj.ok(&j, &["git", "push", "--bookmark", "topic-jj"]);
        branches.push(("topic-jj", jj.show(&j, "topic-jj", "commit_id")));
    }
    let mut out = Vec::new();
    for (b, head) in branches {
        let r = w.api(
            &dave,
            "POST",
            "/merge-requests",
            Some(json!({"source_repo": "dave/demo", "source_branch": b,
                        "title": format!("{b}: {INJECTED}")})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        let n = r.json()["number"].as_i64().unwrap();
        assert_eq!(head.len(), 40, "{head}");
        let target = s.repo_path(&slath_tests::server::DEMO);
        eventually(s, "no mr branch", || {
            git.cmd(&target)
                .args(["rev-parse", "--verify", "-q", &format!("refs/heads/mr/{n}")])
                .output()
                .unwrap()
                .stdout
                .starts_with(head.as_bytes())
        });
        out.push((n, head));
    }
    // Bob's merge request from a branch of the repository passes.
    let r = w.api(
        &w.bob,
        "POST",
        "/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Writer's change"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    (out, r.json()["number"].as_i64().unwrap())
}

#[test]
fn conf_agent_19_outsiders_merge_requests_are_withheld_whole() {
    let w = World::start();
    let r = w.s.client().api(
        "PATCH",
        "/api/v1/repos/alice/demo",
        Some(&w.alice),
        Some(&json!({"visibility": "public"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let (_claimer, task, git, dir) = crate::work_orders::claimed_with(&w, json!({}));
    let (withheld, writers) = outsider_mrs(&w);
    // Involved, so the bot would see their branches.
    for (n, _) in &withheld {
        let r = w.api(
            &w.alice,
            "PUT",
            &format!("/merge-requests/{n}/reviewers"),
            Some(json!({"reviewers": ["claimer", "carol"]})),
        );
        assert!(r.status < 300, "{}", r.body);
    }

    for (n, head) in &withheld {
        let placeholder = json!({"withheld": true, "merge_request": n,
                                 "origin": "outside_contributor", "reason": "content_trust"});
        for sub in [
            "",
            "/diff",
            "/commits",
            "/revisions",
            "/iteration",
            "/state",
        ] {
            let r = w.api(&task, "GET", &format!("/merge-requests/{n}{sub}"), None);
            assert_eq!(r.status, 200, "{sub}: {}", r.body);
            assert_eq!(r.json(), placeholder, "{sub}");
        }
        let body = mcp(
            &w,
            &task,
            "get_merge_request",
            json!({"ns": "alice", "repo": "demo", "number": n}),
        );
        assert!(
            !body.contains(INJECTED) && body.contains("content_trust"),
            "{body}"
        );
        for path in [
            format!("/commits/{head}"),
            format!("/commits/{head}/diff"),
            format!("/commits?ref={head}"),
            format!("/commits?ref=mr/{n}"),
            format!("/tree?ref={head}"),
        ] {
            let r = w.api(&task, "GET", &path, None);
            assert_eq!(r.status, 404, "{path}: {}", r.body);
        }
        // A person reads it all.
        let r = w.api(&w.carol, "GET", &format!("/commits/{head}"), None);
        assert_eq!(r.status, 200, "{}", r.body);
    }
    let list = w.api(&task, "GET", "/merge-requests?state=all", None);
    assert!(!list.body.contains(INJECTED), "{}", list.body);
    let r = w.api(&task, "GET", &format!("/merge-requests/{writers}"), None);
    assert_eq!(r.json()["title"], "Writer's change");

    // Git over HTTPS: not advertised, and not fetched by ID with v2. The
    // repository is public, so the credentials go in a header (Git sends
    // URL credentials only when asked).
    let url = w.s.url(&slath_tests::server::DEMO);
    let url = format!("http://{}", url.split_once('@').unwrap().1);
    let as_user = |token: &str| {
        use base64::Engine;
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("git:{token}"));
        format!("http.extraHeader=Authorization: Basic {basic}")
    };
    let ls = |token: &str| git.ok(dir.path(), &["-c", &as_user(token), "ls-remote", &url]);
    let (refs, carol_refs) = (ls(&task), ls(&w.carol));
    for (n, head) in &withheld {
        assert!(!refs.contains(&format!("refs/heads/mr/{n}")), "{refs}");
        assert!(!refs.contains(head.as_str()), "{head} in {refs}");
        assert!(
            carol_refs.contains(&format!("refs/heads/mr/{n}")),
            "carol: {carol_refs}"
        );
        let fetch = |token: &str| {
            git.cmd(dir.path())
                .args(["-c", &as_user(token), "-c", "protocol.version=2"])
                .args(["fetch", "-q", &url, head])
                .output()
                .unwrap()
                .status
                .success()
        };
        assert!(!fetch(&task), "the task token fetched {head} by ID");
        assert!(fetch(&w.carol), "a reviewer fetches {head}");
    }

    // Every GET operation, as the task token: no outsider text anywhere.
    let spec =
        w.s.client()
            .api("GET", "/api/v1/openapi.json", None, None)
            .json();
    let numbers: Vec<String> = (1..=4).map(|n| n.to_string()).collect();
    let mut checked = 0;
    for (path, item) in spec["paths"].as_object().unwrap() {
        if item.get("get").is_none() {
            continue;
        }
        let mut paths = vec![path.replace("{ns}", "alice").replace("{repo}", "demo")];
        for (param, values) in [
            ("{number}", numbers.clone()),
            ("{oid}", withheld.iter().map(|(_, h)| h.clone()).collect()),
            ("{ref}", vec!["main".to_owned(), "mr/1".to_owned()]),
            ("{user}", vec!["dave".to_owned()]),
        ] {
            paths = paths
                .into_iter()
                .flat_map(|p| {
                    if p.contains(param) {
                        values.iter().map(|v| p.replace(param, v)).collect()
                    } else {
                        vec![p]
                    }
                })
                .collect();
        }
        for p in paths.into_iter().filter(|p| !p.contains('{')) {
            let sep = if p.contains('?') { '&' } else { '?' };
            let p = format!("{p}{sep}q={INJECTED}&state=all");
            let r = w.s.client().api("GET", &p, Some(&task), None);
            assert!(!r.body.contains(INJECTED), "{p}: {}", r.body);
            checked += 1;
        }
    }
    assert!(checked > 60, "only {checked} reads checked");
}

#[test]
fn conf_agent_19_delegating_an_outsiders_merge_request_needs_all() {
    let w = World::start();
    let r = w.s.client().api(
        "PATCH",
        "/api/v1/repos/alice/demo",
        Some(&w.alice),
        Some(&json!({"visibility": "public"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let _claimed = crate::work_orders::claimed_with(&w, json!({}));
    let (withheld, writers) = outsider_mrs(&w);
    let (n, _) = &withheld[0];
    let order = |trust: &str, n: i64| {
        w.delegate(
            &w.bob,
            json!({"target": {"merge_request": n}, "bot": "claimer", "content_trust": trust}),
        )
    };
    for trust in ["members", "writers"] {
        let r = order(trust, *n);
        assert_eq!(r.status, 422, "{}", r.body);
        assert!(r.body.contains("content_trust: all"), "{}", r.body);
        assert!(r.body.contains("outside contributor"), "{}", r.body);
    }
    let r = order("members", writers);
    assert_eq!(r.status, 201, "{}", r.body);
    let r = order("all", *n);
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.json()["content_trust"], "all");
}

#[test]
fn conf_agent_19_the_delegate_form_asks_before_the_bot_reads_an_outsider() {
    use slath_tests::http::submit;
    let w = World::start();
    let r = w.s.client().api(
        "PATCH",
        "/api/v1/repos/alice/demo",
        Some(&w.alice),
        Some(&json!({"visibility": "public"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let _claimed = crate::work_orders::claimed_with(&w, json!({}));
    let (withheld, writers) = outsider_mrs(&w);
    let mut bob = w.s.login("bob", slath_tests::server::PASSWORD);

    // An ordinary target: the level is one choice among the limits.
    let form = format!("/alice/demo/merge-requests/{writers}/delegate");
    let f = bob.get(&form).body;
    assert!(
        f.contains("name=\"content_trust\" value=\"members\" checked"),
        "{f}"
    );
    assert!(!f.contains("read_all"), "{f}");
    let r = submit(
        &mut bob,
        &form,
        &form,
        &[
            ("bot", "claimer"),
            ("goal", "Tidy"),
            ("content_trust", "writers"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let order = r.header("location").unwrap().to_owned();
    assert!(
        bob.get(&order)
            .body
            .contains("<dt>Reads text from</dt><dd>Writers</dd>")
    );

    // An outsider's merge request: a warning and one checkbox instead.
    let form = format!("/alice/demo/merge-requests/{}/delegate", withheld[0].0);
    let f = bob.get(&form).body;
    assert!(f.contains("notice-warning"), "{f}");
    assert!(f.contains("an outside contributor"), "{f}");
    assert!(f.contains("name=\"read_all\""), "{f}");
    assert!(!f.contains("name=\"content_trust\""), "{f}");
    let r = submit(
        &mut bob,
        &form,
        &form,
        &[("bot", "claimer"), ("goal", "Review")],
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(r.body.contains("id=\"content_trust-error\""), "{}", r.body);
    let r = submit(
        &mut bob,
        &form,
        &form,
        &[("bot", "claimer"), ("goal", "Review"), ("read_all", "on")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let order = r.header("location").unwrap().to_owned();
    assert!(
        bob.get(&order)
            .body
            .contains("<dt>Reads text from</dt><dd>Everyone</dd>")
    );
}
