// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fixture generators must be deterministic: identical refs and object IDs on
//! every run, so expected states can be asserted by OID.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use slath_tests::fixtures;

#[test]
fn every_generator_yields_identical_refs_twice() {
    let names = fixtures::names();
    assert!(names.len() >= 8, "missing generators: {names:?}");
    for name in names {
        let tmp = tempfile::tempdir().unwrap();
        let args: &[&str] = if name == "deep_history" {
            &["300"]
        } else {
            &[]
        };
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fixtures::generate(&name, &a, args);
        fixtures::generate(&name, &b, args);
        let refs = fixtures::refs(&a);
        assert!(!refs.is_empty(), "{name}: no refs");
        assert_eq!(refs, fixtures::refs(&b), "{name}: refs differ between runs");
    }
}

#[test]
fn deep_history_has_the_requested_length() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("deep");
    fixtures::generate("deep_history", &repo, &["1234"]);
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["rev-list", "--count", "main"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "1234");
}
