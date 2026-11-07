// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Push protection's scanner: finds secrets in text.
//!
//! The built-in rules are versioned with [`RULES_VERSION`] and published in
//! `docs/admin/push-protection.md`. **Block** rules cover formats that are
//! secrets with high confidence (provider tokens with fixed prefixes,
//! private-key blocks, credentials in URLs, Slath's own tokens); **warn**
//! rules (generic `password = "…"` assignments) never block. Matching uses
//! the `regex` crate, which runs in time linear in the input, and every
//! rule is a single pass over the text.

use std::fmt::Write as _;

use regex::bytes::Regex;
use sha2::{Digest, Sha256};

use crate::credential;

/// Version of the built-in rule set; part of every finding.
pub const RULES_VERSION: &str = "2026.11.1";

/// Whether a finding stops the push.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    Warn,
    Block,
}

impl Tier {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Block => "block",
        }
    }
}

/// A rule: an ID, a description, a tier, and how it matches.
pub struct Rule {
    pub id: String,
    pub description: String,
    pub tier: Tier,
    matcher: Matcher,
}

enum Matcher {
    /// The secret is capture group `secret` (or the whole match). With
    /// `boundary`, a match must not continue a word on its left.
    Regex { re: Regex, boundary: bool },
    /// Slath's own credentials, by prefix and checksum.
    SlathToken,
}

/// A secret found in some text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: String,
    pub description: String,
    pub tier: Tier,
    /// 1-based line and byte column of the secret.
    pub line: usize,
    pub column: usize,
    /// The line with the secret shortened to its first four characters.
    pub excerpt: String,
    /// Hex SHA-256 of the secret (the first 32 digits): the same secret
    /// has the same fingerprint wherever it appears.
    pub fingerprint: String,
}

/// Why the rule set couldn't be built (operator rules from the config).
#[derive(Debug, thiserror::Error)]
#[error("push protection rule {id}: {why}")]
pub struct RuleError {
    pub id: String,
    pub why: String,
}

/// An operator's additional rule (`[[push_protection.rule]]`).
#[derive(Debug, Clone)]
pub struct CustomRule {
    pub id: String,
    pub description: String,
    pub regex: String,
    pub tier: Tier,
}

/// The built-in rules: `(id, tier, description, pattern)`. A pattern's
/// `secret` group is the secret; without one, the whole match is.
const BUILT_IN: &[(&str, Tier, &str, &str)] = &[
    (
        "private-key",
        Tier::Block,
        "Private key",
        r"-----BEGIN (?:RSA |DSA |EC |OPENSSH |ENCRYPTED |PGP )?PRIVATE KEY(?: BLOCK)?-----\r?\n(?:[A-Za-z0-9-]+: [^\r\n]*\r?\n)*\r?\n?(?P<secret>[A-Za-z0-9+/=]{40,})",
    ),
    (
        "github-token",
        Tier::Block,
        "GitHub token",
        r"(?P<secret>(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36})\b",
    ),
    (
        "github-fine-grained-token",
        Tier::Block,
        "GitHub fine-grained token",
        r"(?P<secret>github_pat_[A-Za-z0-9]{22}_[A-Za-z0-9]{59})\b",
    ),
    (
        "gitlab-token",
        Tier::Block,
        "GitLab personal access token",
        r"(?P<secret>glpat-[A-Za-z0-9_-]{20})\b",
    ),
    (
        "aws-access-key-id",
        Tier::Block,
        "AWS access key ID",
        r"(?P<secret>(?:AKIA|ASIA)[A-Z2-7]{16})\b",
    ),
    (
        "slack-token",
        Tier::Block,
        "Slack token",
        r"(?P<secret>xox[abprs]-[0-9]{8,}-[0-9A-Za-z-]{8,})\b",
    ),
    (
        "stripe-secret-key",
        Tier::Block,
        "Stripe secret key",
        r"(?P<secret>(?:sk_live_|rk_live_)[0-9A-Za-z]{24,128})\b",
    ),
    (
        "google-api-key",
        Tier::Block,
        "Google API key",
        r"(?P<secret>AIza[0-9A-Za-z_-]{35})(?:[^0-9A-Za-z_-]|$)",
    ),
    (
        "npm-token",
        Tier::Block,
        "npm access token",
        r"(?P<secret>npm_[A-Za-z0-9]{36})\b",
    ),
    (
        "pypi-token",
        Tier::Block,
        "PyPI API token",
        r"(?P<secret>pypi-AgEIcHlwaS5vcmc[A-Za-z0-9_-]{50,})",
    ),
    (
        "url-credentials",
        Tier::Block,
        "Password in a URL",
        r"[a-z][a-z0-9+.-]{1,20}://[A-Za-z0-9._~%-]{1,64}:(?P<secret>[A-Za-z0-9._~%!*+,;=-]{6,128})@[A-Za-z0-9.-]+",
    ),
    (
        "generic-assignment",
        Tier::Warn,
        "Possible password or key in an assignment",
        r#"(?:[Pp]assword|PASSWORD|[Pp]asswd|PASSWD|[Ss]ecret|SECRET|[Aa]pi_?[Kk]ey|API_?KEY|[Aa]ccess_?[Kk]ey|ACCESS_?KEY|[Aa]uth_?[Tt]oken|AUTH_?TOKEN|[Pp]rivate_?[Kk]ey|PRIVATE_?KEY)\b["']?[ \t]{0,8}[:=][ \t]{0,8}["'](?P<secret>[^"'\s]{12,128})["']"#,
    ),
];

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Values that are examples or placeholders, never secrets.
fn is_placeholder(secret: &[u8]) -> bool {
    let s = String::from_utf8_lossy(secret).to_ascii_lowercase();
    const WORDS: &[&str] = &[
        "example",
        "placeholder",
        "changeme",
        "change_me",
        "redacted",
        "dummy",
        "xxxx",
        "your_",
        "your-",
        "<",
        "${",
        "{{",
        "%(",
        "password",
        "secret",
        "token",
        "*****",
        "....",
    ];
    if WORDS.iter().any(|w| s.contains(w)) || s.starts_with('$') {
        return true;
    }
    // One character repeated, or very few distinct ones: not random.
    let mut distinct = [false; 256];
    for &b in secret {
        distinct[usize::from(b)] = true;
    }
    distinct.iter().filter(|d| **d).count() < 6
}

/// A set of rules, ready to scan.
pub struct RuleSet {
    rules: Vec<Rule>,
}

impl RuleSet {
    /// The built-in rules without `disabled`, plus `custom`.
    pub fn new(disabled: &[String], custom: &[CustomRule]) -> Result<Self, RuleError> {
        let mut rules = vec![Rule {
            id: "slath-token".into(),
            description: "Slath token".into(),
            tier: Tier::Block,
            matcher: Matcher::SlathToken,
        }];
        for (id, tier, description, pattern) in BUILT_IN {
            // ASCII semantics (word boundaries, case folding) keep every
            // rule on the regex crate's fast DFA path.
            let re = regex::bytes::RegexBuilder::new(pattern)
                .unicode(false)
                .build()
                .map_err(|e| RuleError {
                    id: (*id).into(),
                    why: e.to_string(),
                })?;
            rules.push(Rule {
                id: (*id).into(),
                description: (*description).into(),
                tier: *tier,
                matcher: Matcher::Regex {
                    re,
                    boundary: pattern.starts_with("(?P<secret>") || pattern.starts_with("[a-z]"),
                },
            });
        }
        if let Some(unknown) = disabled.iter().find(|d| !rules.iter().any(|r| &r.id == *d)) {
            return Err(RuleError {
                id: unknown.clone(),
                why: "no built-in rule has this ID".into(),
            });
        }
        rules.retain(|r| !disabled.contains(&r.id));
        for c in custom {
            if rules.iter().any(|r| r.id == c.id) || BUILT_IN.iter().any(|b| b.0 == c.id) {
                return Err(RuleError {
                    id: c.id.clone(),
                    why: "the ID is already taken".into(),
                });
            }
            let re = regex::bytes::RegexBuilder::new(&c.regex)
                .size_limit(1 << 20)
                .build()
                .map_err(|e| RuleError {
                    id: c.id.clone(),
                    why: e.to_string(),
                })?;
            rules.push(Rule {
                id: c.id.clone(),
                description: c.description.clone(),
                tier: c.tier,
                matcher: Matcher::Regex {
                    re,
                    boundary: false,
                },
            });
        }
        Ok(Self { rules })
    }

    /// The built-in rules only.
    #[must_use]
    pub fn built_in() -> Self {
        Self::new(&[], &[]).unwrap_or_else(|_| unreachable!("the built-in rules compile"))
    }

    #[must_use]
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Every finding in `text`, ordered by position.
    #[must_use]
    pub fn scan(&self, text: &[u8]) -> Vec<Finding> {
        let mut found: Vec<(usize, usize, &Rule)> = Vec::new();
        for rule in &self.rules {
            match &rule.matcher {
                Matcher::SlathToken => {
                    for f in credential::find_all(text) {
                        found.push((f.start, f.start + credential::TOKEN_LEN, rule));
                    }
                }
                Matcher::Regex { re, boundary } => {
                    for c in re.captures_iter(text) {
                        let Some(m) = c.name("secret").or_else(|| c.get(0)) else {
                            continue;
                        };
                        let start = c.get(0).map_or(m.start(), |w| w.start());
                        // Rules start at a word boundary, checked here so the
                        // patterns keep a literal prefix for fast searching.
                        let ok = !*boundary || start == 0 || !is_word(text[start - 1]);
                        if ok && !is_placeholder(m.as_bytes()) {
                            found.push((m.start(), m.end(), rule));
                        }
                    }
                }
            }
        }
        found.sort_by_key(|(start, _, r)| (*start, std::cmp::Reverse(r.tier)));
        // The same secret matched by several rules counts once (the strictest).
        found.dedup_by_key(|(start, _, _)| *start);
        found
            .into_iter()
            .map(|(start, end, rule)| finding(text, start, end, rule))
            .collect()
    }
}

fn finding(text: &[u8], start: usize, end: usize, rule: &Rule) -> Finding {
    let line_start = text[..start]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |p| p + 1);
    let line_end = text[end..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(text.len(), |p| end + p);
    let line = 1 + text[..start].iter().filter(|&&b| b == b'\n').count();
    let keep = (start + 4).min(end);
    let mut excerpt = String::from_utf8_lossy(&text[line_start..keep]).into_owned();
    excerpt.push('…');
    let tail = String::from_utf8_lossy(&text[end..line_end]);
    excerpt.push_str(&tail);
    let excerpt: String = excerpt
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(160)
        .collect();
    let digest = Sha256::digest(&text[start..end]);
    let mut fingerprint = String::with_capacity(32);
    for b in &digest[..16] {
        let _ = write!(fingerprint, "{b:02x}");
    }
    Finding {
        rule: rule.id.clone(),
        description: rule.description.clone(),
        tier: rule.tier,
        line,
        column: start - line_start + 1,
        excerpt,
        fingerprint,
    }
}

/// True if `data` looks binary: a NUL byte in its first 8000 bytes, as Git
/// decides it. Binary files aren't scanned.
#[must_use]
pub fn is_binary(data: &[u8]) -> bool {
    data[..data.len().min(8000)].contains(&0)
}

/// One positive sample per built-in rule, for tests: none of them is a
/// real credential.
#[must_use]
pub fn samples() -> Vec<(&'static str, String)> {
    let slath = credential::assemble(
        credential::CredentialKind::PersonalAccessToken,
        "q7Rt2LmX9vKp4Zs8Hd1Nw6Bc3Fy5Jg0Ta2Qe7Ui9Ok4",
    )
    .unwrap_or_default();
    vec![
        ("slath-token", format!("SLATH_TOKEN={slath}\n")),
        ("private-key", "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtz\n-----END OPENSSH PRIVATE KEY-----\n".into()),
        ("github-token", "token: ghp_Zq3Rt7Lm2Xv9Kp4Ws8Hd1Nb6Cf3Jy5Gu0Ta2\n".into()),
        ("github-fine-grained-token", "github_pat_11ABCDEFG0aZq3Rt7Lm2Xv_9Kp4Ws8Hd1Nb6Cf3Jy5Gu0Ta2Qe7Ui9Ok4Pl6Mn1Bv8Cx3Zd5Fs7Gh2Jk4L\n".into()),
        ("gitlab-token", "GITLAB=glpat-Zq3Rt7Lm2Xv9Kp4Ws8Hd\n".into()),
        ("aws-access-key-id", "aws_access_key_id = AKIA2ZQ3RT7LM2XV6KP4\n".into()),
        ("slack-token", "SLACK=xoxb-12345678901-Zq3Rt7Lm2Xv9Kp4Ws8\n".into()),
        ("stripe-secret-key", "stripe: sk_live_Zq3Rt7Lm2Xv9Kp4Ws8Hd1Nb6C\n".into()),
        ("google-api-key", "key=AIzaSyZq3Rt7Lm2Xv9Kp4Ws8Hd1Nb6Cf3Jy5Gu0\n".into()),
        ("npm-token", "//registry.npmjs.org/:_authToken=npm_Zq3Rt7Lm2Xv9Kp4Ws8Hd1Nb6Cf3Jy5Gu0Ta2\n".into()),
        ("pypi-token", "password = pypi-AgEIcHlwaS5vcmcZq3Rt7Lm2Xv9Kp4Ws8Hd1Nb6Cf3Jy5Gu0Ta2Qe7Ui9Ok4Pl6Mn1\n".into()),
        ("url-credentials", "remote = https://deploy:Zq3Rt7Lm2Xv9@git.internal.example.net/app.git\n".into()),
        ("generic-assignment", "db_password = \"Zq3Rt7Lm2Xv9Kp4W\"\n".into()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules_hit(text: &str) -> Vec<String> {
        RuleSet::built_in()
            .scan(text.as_bytes())
            .into_iter()
            .map(|f| f.rule)
            .collect()
    }

    #[test]
    fn every_built_in_rule_has_a_sample_and_finds_it() {
        let samples = samples();
        for rule in RuleSet::built_in().rules() {
            let sample = samples
                .iter()
                .find(|(id, _)| *id == rule.id)
                .unwrap_or_else(|| panic!("no sample for {}", rule.id));
            assert_eq!(rules_hit(&sample.1), vec![rule.id.clone()], "{}", sample.1);
        }
    }

    #[test]
    fn placeholders_and_examples_are_not_secrets() {
        for text in [
            "token = ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            "AKIAIOSFODNN7EXAMPLE",
            "postgres://user:password@localhost/db",
            "postgres://slath:${DB_PASSWORD}@postgres/slath",
            "https://user:<token>@example.org/repo.git",
            "-----BEGIN PRIVATE KEY-----\n...\n-----END PRIVATE KEY-----",
            "password = \"correct horse battery staple\"",
            "slath_pat_AbCdEfGh…",
        ] {
            let hits: Vec<_> = RuleSet::built_in()
                .scan(text.as_bytes())
                .into_iter()
                .filter(|f| f.tier == Tier::Block)
                .collect();
            assert!(hits.is_empty(), "{text}: {hits:?}");
        }
    }

    #[test]
    fn findings_locate_and_redact() {
        let text = "a\nb = 1\nexport KEY=ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8 # ci\n";
        let f = &RuleSet::built_in().scan(text.as_bytes())[0];
        assert_eq!((f.line, f.column), (3, 12));
        assert_eq!(f.excerpt, "export KEY=ghp_… # ci");
        assert!(!f.excerpt.contains("A1b2"));
        assert_eq!(f.fingerprint.len(), 32);
        // Same secret elsewhere: same fingerprint.
        let g = &RuleSet::built_in().scan(b"ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8")[0];
        assert_eq!(f.fingerprint, g.fingerprint);
    }

    #[test]
    fn operators_disable_and_add_rules() {
        let custom = CustomRule {
            id: "acme-key".into(),
            description: "ACME key".into(),
            regex: r"acme_[0-9a-f]{16}".into(),
            tier: Tier::Block,
        };
        let set = RuleSet::new(&["github-token".into()], std::slice::from_ref(&custom)).unwrap();
        assert!(
            set.scan(b"ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8")
                .is_empty()
        );
        assert_eq!(set.scan(b"key acme_0123456789abcdef")[0].rule, "acme-key");
        assert!(RuleSet::new(&["nope".into()], &[]).is_err());
        let dup = CustomRule {
            id: "github-token".into(),
            ..custom.clone()
        };
        assert!(RuleSet::new(&[], &[dup]).is_err());
        let bad = CustomRule {
            regex: "(".into(),
            ..custom
        };
        assert!(RuleSet::new(&[], &[bad]).is_err());
    }

    #[test]
    fn binary_detection() {
        assert!(is_binary(b"\x89PNG\r\n\x1a\n\0\0"));
        assert!(!is_binary(b"plain text"));
    }
}
