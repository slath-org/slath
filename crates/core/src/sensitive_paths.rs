// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Sensitive paths: files that steer what runs, who reviews, what the
//! secret scanner allows, or what agents are told. Bots can't change them,
//! and on a protected target a merge request that changes them needs a
//! maintainer's approval.

use crate::secret_scan::glob_match;

/// Built-in globs; they always apply, whatever a repository adds.
pub const DEFAULTS: &[&str] = &[
    // CI workflows.
    ".slath/workflows/**",
    ".github/workflows/**",
    ".gitea/workflows/**",
    ".forgejo/workflows/**",
    // Slath's own files: the reviewer file, the secret-scan allowlist.
    ".slath/**",
    // Code owners.
    "CODEOWNERS",
    ".github/CODEOWNERS",
    ".gitea/CODEOWNERS",
    "docs/CODEOWNERS",
    // Agent instructions.
    "**/AGENTS.md",
    "**/AGENT.md",
    "**/CLAUDE.md",
    "**/GEMINI.md",
    ".github/copilot-instructions.md",
    ".github/instructions/**",
    ".cursorrules",
    ".cursor/rules/**",
    ".windsurfrules",
    ".clinerules",
    "REVIEW.md",
];

/// Most globs a repository may add.
pub const MAX_EXTRA: usize = 100;
/// Longest glob a repository may add, in bytes.
pub const MAX_GLOB_BYTES: usize = 255;

/// Why a repository's glob is refused, if it is.
#[must_use]
pub fn invalid_glob(glob: &str) -> Option<&'static str> {
    if glob.is_empty() {
        return Some("is empty");
    }
    if glob.len() > MAX_GLOB_BYTES {
        return Some("is longer than 255 bytes");
    }
    if glob.starts_with('/') || glob.ends_with('/') {
        return Some("must not start or end with /");
    }
    if glob.chars().any(char::is_control) {
        return Some("contains control characters");
    }
    if glob
        .split('/')
        .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return Some("has an empty, . or .. segment");
    }
    None
}

/// The defaults plus a repository's own globs.
#[derive(Debug, Clone, Default)]
pub struct SensitivePaths {
    extra: Vec<String>,
}

impl SensitivePaths {
    #[must_use]
    pub fn new(extra: Vec<String>) -> Self {
        Self { extra }
    }

    /// The repository's own globs.
    #[must_use]
    pub fn extra(&self) -> &[String] {
        &self.extra
    }

    /// Whether `path` (from the repository root) is sensitive. ASCII case
    /// doesn't matter: some file systems and tools treat `agents.md` as
    /// `AGENTS.md`.
    #[must_use]
    pub fn matches(&self, path: &[u8]) -> bool {
        let path = path.to_ascii_lowercase();
        DEFAULTS
            .iter()
            .copied()
            .chain(self.extra.iter().map(String::as_str))
            .any(|g| glob_match(g.to_ascii_lowercase().as_bytes(), &path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_each_kind() {
        let s = SensitivePaths::default();
        for p in [
            ".slath/workflows/ci.yml",
            ".github/workflows/release.yaml",
            ".gitea/workflows/a/b.yml",
            ".forgejo/workflows/x.yml",
            ".slath/REVIEWERS",
            ".slath/secret-scan.toml",
            "CODEOWNERS",
            ".github/CODEOWNERS",
            "AGENTS.md",
            "crates/core/AGENTS.md",
            "CLAUDE.md",
            "deep/down/claude.md",
            "GEMINI.md",
            ".github/copilot-instructions.md",
            ".github/instructions/rust.instructions.md",
            ".cursorrules",
            ".cursor/rules/style.mdc",
            "REVIEW.md",
            "agents.md",
        ] {
            assert!(s.matches(p.as_bytes()), "{p}");
        }
        for p in [
            "README.md",
            "src/workflows/ci.yml",
            "docs/AGENTS.md.bak",
            "my-AGENTS.md",
            "github/workflows/ci.yml",
            "sub/CODEOWNERS",
            "sub/REVIEW.md",
        ] {
            assert!(!s.matches(p.as_bytes()), "{p}");
        }
    }

    #[test]
    fn repositories_add_globs() {
        let s = SensitivePaths::new(vec!["deploy/**".into(), "Makefile".into()]);
        assert!(s.matches(b"deploy/prod/values.yaml"));
        assert!(s.matches(b"makefile"));
        assert!(s.matches(b"AGENTS.md"), "defaults stay");
        assert!(!s.matches(b"src/deploy.rs"));
    }

    #[test]
    fn globs_are_checked() {
        for bad in ["", "/abs", "dir/", "a//b", "../up", "a/./b", "x\ny"] {
            assert!(invalid_glob(bad).is_some(), "{bad:?}");
        }
        assert!(invalid_glob(&"a".repeat(256)).is_some());
        for good in ["deploy/**", "*.tf", "infra/?/main.tf", "Makefile"] {
            assert_eq!(invalid_glob(good), None, "{good}");
        }
    }
}
