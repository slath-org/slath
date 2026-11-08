// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Push protection's rules against the false-positive corpus, the
//! project's own sources, the published rule list, and a time budget.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::time::Instant;

use slath_core::secret_scan::{RuleSet, Tier, samples};

fn root() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn blocking(text: &[u8]) -> Vec<String> {
    RuleSet::built_in()
        .scan(text)
        .into_iter()
        .filter(|f| f.tier == Tier::Block)
        .map(|f| format!("{} line {}: {}", f.rule, f.line, f.excerpt))
        .collect()
}

#[test]
fn conf_sec_08_the_false_positive_corpus_has_no_block_findings() {
    let corpus =
        std::fs::read(root().join("tests/fixtures/secret-scan/false-positives.txt")).unwrap();
    assert_eq!(blocking(&corpus), Vec::<String>::new());
}

#[test]
fn conf_sec_08_docs_have_no_block_findings() {
    for dir in ["docs/admin", "docs/dev", "deploy"] {
        for entry in std::fs::read_dir(root().join(dir)).unwrap().flatten() {
            if entry.file_type().unwrap().is_file() {
                let text = std::fs::read(entry.path()).unwrap();
                assert_eq!(
                    blocking(&text),
                    Vec::<String>::new(),
                    "{}",
                    entry.path().display()
                );
            }
        }
    }
}

#[test]
fn every_rule_is_published() {
    let doc = std::fs::read_to_string(root().join("docs/admin/push-protection.md")).unwrap();
    for rule in RuleSet::built_in().rules() {
        assert!(
            doc.contains(&format!("`{}`", rule.id)),
            "{} is missing from the docs",
            rule.id
        );
    }
    assert!(doc.contains(slath_core::secret_scan::RULES_VERSION));
    assert_eq!(samples().len(), RuleSet::built_in().rules().len());
}

#[test]
fn scanning_stays_within_its_time_budget() {
    // About 16 MiB of ordinary source text with one secret at the end.
    let mut text = Vec::new();
    for f in [
        "crates/core/src/config.rs",
        "crates/service/src/push.rs",
        "docs/admin/install.md",
    ] {
        text.extend(std::fs::read(root().join(f)).unwrap());
    }
    let unit = text.clone();
    while text.len() < 16 << 20 {
        text.extend_from_slice(&unit);
    }
    text.extend_from_slice(samples()[2].1.as_bytes());
    let rules = RuleSet::built_in();
    let t = Instant::now();
    let found = rules.scan(&text);
    let took = t.elapsed();
    assert!(found.iter().any(|f| f.tier == Tier::Block), "{found:?}");
    // The budget: 16 MiB in under three seconds, even in debug builds and
    // with the rest of the test suite running in parallel (alone it takes
    // well under half a second).
    assert!(took.as_secs_f64() < 3.0, "16 MiB took {took:?}");
}
