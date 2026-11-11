// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! One UI for Git and jj (JJ-11, D-44): the same merge request made with
//! Git and with jj renders the same pages apart from command blocks, and
//! command blocks follow the viewer's tool: their setting, else what their
//! pushes came from, else Git (switchable on the page at any time).

use std::path::Path;

use regex::Regex;
use serde_json::json;
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

use crate::fork_merge_requests::{api, eventually, token};
use crate::jj::{Jj, jj_binaries};

/// A page with everything that legitimately differs between the two merge
/// requests replaced: IDs, numbers, branch names, times, and the command
/// blocks.
fn normalized(html: &str, branch: &str, n: i64) -> String {
    let mut s = html.replace(branch, "BRANCH");
    for (re, by) in [
        // The checkout menu's commands differ by design.
        (
            r#"(?s)<details class="menu checkout-menu.*?</details>"#,
            "CHECKOUT",
        ),
        (r"\b[0-9a-f]{7,40}\b", "OID"),
        (r"\b[k-z]{8,32}\b", "CHANGE"),
        (r#"datetime="[^"]*""#, "datetime"),
        (r"\d{4}-\d\d-\d\d \d\d:\d\d UTC", "TIME"),
        (r#"name="_csrf" value="[^"]*""#, "csrf"),
        (r#"nonce="[^"]*""#, "nonce"),
    ] {
        s = Regex::new(re).unwrap().replace_all(&s, by).into_owned();
    }
    s.replace(&format!("/merge-requests/{n}"), "/merge-requests/N")
        .replace(&format!("!{n}"), "!N")
        .replace(&format!("data-item=\"{n}\""), "data-item=\"N\"")
}

fn write(dir: &Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
}

#[test]
fn conf_jj_11_the_same_merge_request_from_git_and_jj_renders_the_same() {
    let Some((version, bin)) = jj_binaries().into_iter().last() else {
        eprintln!("no jj: skipping");
        return;
    };
    let s = TestServer::start();
    let alice = token(&s, "alice");
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    let url = s.url(&DEMO);
    std::fs::create_dir_all(&w).unwrap();
    let id = ["-c", "user.name=Jay Jay", "-c", "user.email=jj@example.org"];
    let commit = |msg: &str| {
        git.ok(&w, &["add", "-A"]);
        git.ok(&w, &[&id[..], &["commit", "-q", "-m", msg]].concat());
    };
    git.ok(&w, &["init", "-q", "-b", "main"]);
    write(&w, "README", "demo\n");
    commit("init");
    git.ok(&w, &["push", "-q", &url, "main"]);
    // With Git: two commits.
    git.ok(&w, &["switch", "-q", "-c", "made-with-git"]);
    write(&w, "a.txt", "one\ntwo\n");
    commit("Add a");
    write(&w, "b.txt", "bee\n");
    commit("Add b");
    git.ok(&w, &["push", "-q", &url, "made-with-git"]);
    // With jj: the same two changes.
    let jj = Jj::new(&version, &bin);
    let j = d.path().join("jj");
    jj.ok(d.path(), &["git", "clone", &url, j.to_str().unwrap()]);
    jj.ok(&j, &["new", "main@origin", "-m", "Add a"]);
    write(&j, "a.txt", "one\ntwo\n");
    jj.ok(&j, &["new", "-m", "Add b"]);
    write(&j, "b.txt", "bee\n");
    jj.ok(&j, &["bookmark", "create", "made-with-jj", "-r", "@"]);
    jj.ok(&j, &["git", "push", "--bookmark", "made-with-jj"]);

    let mut numbers = Vec::new();
    for b in ["made-with-git", "made-with-jj"] {
        let r = api(
            &s,
            &alice,
            "POST",
            "/alice/demo/merge-requests",
            Some(json!({"source_branch": b, "title": "Add a and b", "description": "Two files."})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        let n = r.json()["number"].as_i64().unwrap();
        eventually(&s, "no first revision", || {
            api(
                &s,
                &alice,
                "GET",
                &format!("/alice/demo/merge-requests/{n}"),
                None,
            )
            .json()["revisions"]
                == 1
        });
        numbers.push((b, n));
    }
    // The API agrees apart from names and IDs.
    let stack = |n: i64| {
        let v = api(
            &s,
            &alice,
            "GET",
            &format!("/alice/demo/merge-requests/{n}/stack"),
            None,
        )
        .json();
        normalized(&v.to_string(), "made-with-", n)
    };
    assert_eq!(stack(numbers[0].1), stack(numbers[1].1));
    // Every tab of the merge request page, as a visitor and as alice.
    let mut alice_web = s.login("alice", PASSWORD);
    for tab in ["", "/commits", "/changes"] {
        let page = |c: &mut slath_tests::http::Client, (b, n): (&str, i64)| {
            let r = c.get(&format!("/alice/demo/merge-requests/{n}{tab}"));
            assert_eq!(r.status, 200, "{b}{tab}");
            normalized(&r.body, b, n)
        };
        let (g, jj_page) = (
            page(&mut alice_web, numbers[0]),
            page(&mut alice_web, numbers[1]),
        );
        if g != jj_page {
            let at = g
                .chars()
                .zip(jj_page.chars())
                .take_while(|(a, b)| a == b)
                .count();
            let ctx = |p: &str| {
                p.chars()
                    .skip(at.saturating_sub(200))
                    .take(400)
                    .collect::<String>()
            };
            panic!(
                "tab {tab:?} differs:\ngit: {}\njj:  {}",
                ctx(&g),
                ctx(&jj_page)
            );
        }
    }
}

#[test]
fn conf_jj_11_commands_follow_the_viewers_tool() {
    let Some((version, bin)) = jj_binaries().into_iter().last() else {
        eprintln!("no jj: skipping");
        return;
    };
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    let alice = token(&s, "alice");
    for (method, path, body) in [
        (
            "PUT",
            "/alice/demo/collaborators/bob",
            json!({"role": "write"}),
        ),
        ("PATCH", "/alice/demo", json!({"visibility": "public"})),
    ] {
        let r = api(&s, &alice, method, path, Some(body));
        assert!(r.status < 300, "{}", r.body);
    }
    let bob = token(&s, "bob");
    let bob_url = format!(
        "http://git:{bob}@{}/alice/demo.git",
        s.base().trim_start_matches("http://")
    );
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    write(&w, "README", "demo\n");
    git.ok(&w, &["add", "-A"]);
    git.ok(&w, &["commit", "-q", "-m", "init"]);
    git.ok(&w, &["push", "-q", &s.url(&DEMO), "main"]);
    git.ok(&w, &["switch", "-q", "-c", "topic"]);
    write(&w, "t.txt", "t\n");
    git.ok(&w, &["add", "-A"]);
    git.ok(&w, &["commit", "-q", "-m", "Topic"]);
    git.ok(&w, &["push", "-q", &s.url(&DEMO), "topic"]);
    let r = api(
        &s,
        &alice,
        "POST",
        "/alice/demo/merge-requests",
        Some(json!({"source_branch": "topic", "title": "Topic"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let page = format!("/alice/demo/merge-requests/{}", r.json()["number"]);
    let checked = |html: &str| -> (bool, bool, bool) {
        let git = html.contains(r#"value="git" checked"#);
        let jj = html.contains(r#"value="jj" checked"#);
        let remember = html.contains(r#"data-remember="tool""#);
        (git, jj, remember)
    };
    // Unknown (no pushes yet): Git, and the browser remembers a switch.
    let mut web = s.login("bob", PASSWORD);
    assert_eq!(checked(&web.get(&page).body), (true, false, true));
    assert_eq!(checked(&s.client().get(&page).body), (true, false, true));
    // A push with Git makes it Git.
    git.ok(&w, &["switch", "-q", "-c", "bob-git", "main"]);
    write(&w, "g.txt", "g\n");
    git.ok(&w, &["add", "-A"]);
    git.ok(&w, &["commit", "-q", "-m", "Bob with Git"]);
    git.ok(&w, &["push", "-q", &bob_url, "bob-git"]);
    eventually(&s, "no Git default", || {
        checked(&web.get(&page).body) == (true, false, false)
    });
    // A push with jj makes it jj.
    let jj = Jj::new(&version, &bin);
    let j = d.path().join("jj");
    jj.ok(d.path(), &["git", "clone", &bob_url, j.to_str().unwrap()]);
    jj.ok(&j, &["new", "main@origin", "-m", "Bob with jj"]);
    write(&j, "j.txt", "j\n");
    jj.ok(&j, &["bookmark", "create", "bob-jj", "-r", "@"]);
    jj.ok(&j, &["git", "push", "--bookmark", "bob-jj"]);
    eventually(&s, "no jj default", || {
        checked(&web.get(&page).body) == (false, true, false)
    });
    // The setting wins.
    let settings = "/-/settings/appearance";
    let r = slath_tests::http::submit(
        &mut web,
        settings,
        settings,
        &[
            ("theme", "system"),
            ("density", "comfortable"),
            ("code_font_size", "14"),
            ("tool", "git"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(checked(&web.get(&page).body), (true, false, false));
    let body = web.get(settings).body;
    assert!(
        body.contains(r#"id="tool-git" type="radio" name="tool" value="git" checked"#),
        "{body}"
    );
    assert!(
        body.contains("Automatic (jj, from your recent pushes)"),
        "{body}"
    );
}
