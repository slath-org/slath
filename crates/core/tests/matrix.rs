// SPDX-FileCopyrightText: 2026 Erich Seifert
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Table-driven test of `authz::check` against `matrix.csv`.
//!
//! Every row is one cell of the authorization matrix. A policy change without a
//! matching CSV change fails here.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use slath_core::authz::{
    Action, Credential, Decision, RepoState, ResourceCtx, Role, Subject, UserKind, UserState,
    Visibility, check,
};
use slath_core::ids::{RepoId, UserId};

struct Row {
    line: usize,
    action: Action,
    ctx: ResourceCtx,
    subject: Subject,
    expect: Decision,
}

fn user(kind: UserKind, state: UserState) -> Subject {
    Subject::User {
        id: UserId::new(),
        kind,
        state,
        is_site_admin: false,
        credential: Credential::Session,
    }
}

fn parse_rows(csv: &str) -> Vec<Row> {
    let mut lines = csv
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !l.starts_with('#'));
    let (_, header) = lines.next().expect("header");
    assert_eq!(header, "action,visibility,state,subject,role,expect");
    lines
        .map(|(i, l)| {
            let f: Vec<&str> = l.split(',').collect();
            assert_eq!(f.len(), 6, "line {}: expected 6 fields", i + 1);
            let action = f[0]
                .parse()
                .unwrap_or_else(|_| panic!("line {}: action", i + 1));
            let visibility = match f[1] {
                "public" => Visibility::Public,
                "private" => Visibility::Private,
                v => panic!("line {}: visibility {v}", i + 1),
            };
            let state = match f[2] {
                "active" => RepoState::Active,
                "archived" => RepoState::Archived,
                "deleting" => RepoState::Deleting,
                v => panic!("line {}: state {v}", i + 1),
            };
            let subject = match f[3] {
                "anon" => Subject::Anonymous,
                "user" => user(UserKind::Human, UserState::Active),
                "suspended" => user(UserKind::Human, UserState::Suspended),
                "bot" => user(UserKind::Bot, UserState::Active),
                v => panic!("line {}: subject {v}", i + 1),
            };
            let role = match f[4] {
                "none" => None,
                r => Some(
                    r.parse::<Role>()
                        .unwrap_or_else(|_| panic!("line {}: role", i + 1)),
                ),
            };
            let expect = match f[5] {
                "allow" => Decision::Allow,
                "deny_hidden" => Decision::Deny { visible: false },
                "deny_visible" => Decision::Deny { visible: true },
                v => panic!("line {}: expect {v}", i + 1),
            };
            Row {
                line: i + 1,
                action,
                ctx: ResourceCtx {
                    repo_id: RepoId::new(),
                    visibility,
                    state,
                    role,
                },
                subject,
                expect,
            }
        })
        .collect()
}

#[test]
fn matrix_matches_policy() {
    let rows = parse_rows(include_str!("matrix.csv"));
    assert!(!rows.is_empty());
    let failures: Vec<String> = rows
        .iter()
        .filter_map(|r| {
            let got = check(&r.subject, r.action, &r.ctx);
            (got != r.expect).then(|| {
                format!(
                    "matrix.csv:{}: {} → {got:?}, expected {:?}",
                    r.line, r.action, r.expect
                )
            })
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
