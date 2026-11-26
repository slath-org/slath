// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Protection rules on facts: facts of what would land that need an
//! approval by a person, and every changed file viewed by a counting
//! approver. Under the one-maintainer policy, the maintainer's review of
//! the exact head meets both once it viewed every file.

use crate::facts::Facts;
use crate::mr_reviews::{Coverage, Requirement, met, unmet};
use crate::one_maintainer::PolicyReview;
use crate::protections::fact_rule_phrase;

/// Whether `f` has the fact `rule` names.
fn has(f: &Facts, rule: &str) -> bool {
    match rule {
        "outside_scope" => f.outside_scope.as_ref().is_some_and(|v| !v.is_empty()),
        "dependencies" => !f.dependencies.is_empty(),
        "deleted_tests" => !f.deleted_tests.is_empty(),
        "added_binaries" => !f.added_binaries.is_empty(),
        "mode_changes" => !f.mode_changes.is_empty(),
        "secrets" => !f.secrets.is_empty(),
        _ => false,
    }
}

/// "Changes dependencies, deletes tests and adds binaries".
fn sentence(phrases: &[&str]) -> String {
    let mut s = match phrases {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    if let Some(c) = s.get(..1) {
        s.replace_range(..1, &c.to_ascii_uppercase());
    }
    s
}

/// The `facts` requirement for `rules` on `facts`: `None` when none of the
/// rules' facts apply. `people` approve every change being merged; the
/// flag says the one-maintainer policy met it.
pub(crate) fn facts_requirement(
    rules: &[String],
    facts: &Facts,
    people: &[String],
    policy: Option<&PolicyReview>,
) -> Option<(Requirement, bool)> {
    let found: Vec<&str> = rules
        .iter()
        .filter(|r| has(facts, r))
        .map(|r| fact_rule_phrase(r))
        .collect();
    if found.is_empty() {
        return None;
    }
    let what = sentence(&found);
    Some(match (people.is_empty(), policy) {
        (false, _) => (
            met(
                "facts",
                format!("{what}; approved by {}.", people.join(", ")),
            ),
            false,
        ),
        (true, Some(p)) if p.complete() => (
            met(
                "facts",
                format!(
                    "{what}; reviewed by {} under the one-maintainer policy with every file viewed.",
                    p.name
                ),
            ),
            true,
        ),
        (true, Some(p)) => (
            unmet(
                "facts",
                format!(
                    "{what}. Under the one-maintainer policy, {}'s review of this revision with every file viewed counts.",
                    p.name
                ),
                "Review every file",
            ),
            false,
        ),
        (true, None) => (
            unmet(
                "facts",
                format!("{what}. Needs an approval by a person."),
                "Ask a person to review",
            ),
            false,
        ),
    })
}

/// The `viewed_files` requirement from the counting approvals' coverage;
/// the flag says the one-maintainer policy met it.
pub(crate) fn viewed_requirement(
    coverage: Option<&Coverage>,
    policy: Option<&PolicyReview>,
) -> (Requirement, bool) {
    if let Some(c) = coverage.filter(|c| !c.approvals.is_empty()) {
        if c.unviewed.is_empty() {
            return (
                met(
                    "viewed_files",
                    "Every changed file was viewed by an approver.",
                ),
                false,
            );
        }
        if !policy.is_some_and(PolicyReview::complete) {
            let mut list: Vec<&str> = c.unviewed.iter().take(10).map(String::as_str).collect();
            let more = c.unviewed.len().saturating_sub(10);
            let tail = if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            };
            let n = c.unviewed.len();
            list.sort_unstable();
            return (
                unmet(
                    "viewed_files",
                    format!(
                        "{n} changed {} viewed by an approver: {}{tail}.",
                        if n == 1 {
                            "file wasn't"
                        } else {
                            "files weren't"
                        },
                        list.join(", ")
                    ),
                    "Ask an approver to view them and approve again",
                ),
                false,
            );
        }
    }
    match policy {
        Some(p) if p.complete() => (
            met(
                "viewed_files",
                format!(
                    "{} viewed every changed file, under the one-maintainer policy.",
                    p.name
                ),
            ),
            true,
        ),
        _ => (
            unmet(
                "viewed_files",
                "Every changed file must be viewed by an approver; no approval counts yet.",
                "Ask a reviewer to view every file and approve",
            ),
            false,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::DependencyFact;

    fn deps() -> Facts {
        Facts {
            dependencies: vec![DependencyFact {
                path: "Cargo.lock".into(),
                ecosystem: "cargo",
                packages: true,
                changes: slath_core::facts::PackageChanges::default(),
            }],
            deleted_tests: vec!["tests/a.rs".into()],
            ..Facts::default()
        }
    }

    fn rules(r: &[&str]) -> Vec<String> {
        r.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn only_the_rules_facts_need_a_person() {
        // A fact the rules don't name changes nothing.
        assert!(facts_requirement(&rules(&["secrets"]), &deps(), &[], None).is_none());
        let (r, policy) = facts_requirement(
            &rules(&["dependencies", "deleted_tests", "secrets"]),
            &deps(),
            &[],
            None,
        )
        .unwrap();
        assert_eq!(r.state, "unmet");
        assert!(!policy);
        assert_eq!(
            r.reason,
            "Changes dependencies and deletes tests. Needs an approval by a person."
        );
        let (r, _) =
            facts_requirement(&rules(&["dependencies"]), &deps(), &["bob".into()], None).unwrap();
        assert_eq!(
            (r.state, r.reason.as_str()),
            ("met", "Changes dependencies; approved by bob.")
        );
    }

    #[test]
    fn every_file_viewed() {
        let (r, _) = viewed_requirement(None, None);
        assert_eq!(r.state, "unmet");
        let c = Coverage {
            approvals: vec![crate::mr_reviews::ApprovalCoverage {
                reviewer: "bob".into(),
                revision: 1,
                files_viewed: 1,
                files_total: 2,
                hunks: None,
            }],
            unviewed: vec!["b.rs".into()],
        };
        let (r, _) = viewed_requirement(Some(&c), None);
        assert_eq!(
            r.reason,
            "1 changed file wasn't viewed by an approver: b.rs."
        );
        let all = Coverage {
            unviewed: Vec::new(),
            ..c
        };
        assert_eq!(viewed_requirement(Some(&all), None).0.state, "met");
    }
}
