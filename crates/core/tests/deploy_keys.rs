// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deploy keys in `authz::check`: a key reads its one repository and, with
//! write access, pushes to it. Every other action is refused there, and
//! every other repository (forks included) doesn't exist for it, whatever
//! its visibility. Checked over every action, visibility, and state.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use slath_core::authz::{Action, Decision, RepoState, ResourceCtx, Subject, Visibility, check};
use slath_core::ids::{RepoId, SshKeyId};

const VISIBILITIES: [Visibility; 3] = [
    Visibility::Public,
    Visibility::Internal,
    Visibility::Private,
];
const STATES: [RepoState; 4] = [
    RepoState::Active,
    RepoState::Archived,
    RepoState::Deleting,
    RepoState::Locked,
];

fn key(repo: RepoId, write: bool) -> Subject {
    Subject::DeployKey {
        id: SshKeyId::new(),
        repo_id: repo,
        write,
    }
}

/// What a deploy key may do on its own repository.
fn expected(action: Action, state: RepoState, write: bool) -> Decision {
    match state {
        RepoState::Deleting | RepoState::Locked => return Decision::Deny { visible: false },
        RepoState::Active | RepoState::Archived => {}
    }
    let allowed = match action {
        Action::RepoRead => true,
        Action::RepoPush => write && state == RepoState::Active,
        _ => false,
    };
    if allowed {
        Decision::Allow
    } else {
        Decision::Deny { visible: true }
    }
}

#[test]
fn conf_rbac_01_deploy_keys_reach_their_repository_only() {
    let own = RepoId::new();
    let other = RepoId::new();
    for write in [false, true] {
        let subject = key(own, write);
        for &action in Action::ALL {
            for visibility in VISIBILITIES {
                for state in STATES {
                    // Whatever role the context claims, the key decides.
                    for role in [None, Some(slath_core::authz::Role::Admin)] {
                        let mine = ResourceCtx::new(own, visibility, state, role);
                        assert_eq!(
                            check(&subject, action, &mine),
                            expected(action, state, write),
                            "{action:?} on its own {visibility:?} {state:?} repository, write={write}, role={role:?}"
                        );
                        let elsewhere = ResourceCtx::new(other, visibility, state, role);
                        assert_eq!(
                            check(&subject, action, &elsewhere),
                            Decision::Deny { visible: false },
                            "{action:?} on another {visibility:?} {state:?} repository, write={write}"
                        );
                    }
                }
            }
        }
    }
}
