// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The format of Slath's own bearer credentials.
//!
//! Every credential is `slath_<kind>_` followed by 43 random base62
//! characters (256 bits) and a 6-character base62 checksum: the CRC-32 of
//! everything before it. The prefix tells a reader what leaked, and the
//! checksum lets a server reject typos and fakes without a database lookup
//! and lets the push scanner find real tokens without false positives.
//!
//! Generation needs randomness and lives in `slath-auth`; this module only
//! formats, checks, and finds.

/// The kinds of Slath credentials. Each has its own fixed prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CredentialKind {
    /// A personal access token of a person.
    PersonalAccessToken,
    /// A personal access token of a bot account.
    BotToken,
    /// A token for a single work-order task.
    TaskToken,
    /// A CI runner's token.
    RunnerToken,
}

impl CredentialKind {
    pub const ALL: [Self; 4] = [
        Self::PersonalAccessToken,
        Self::BotToken,
        Self::TaskToken,
        Self::RunnerToken,
    ];

    /// The fixed prefix, for example `slath_pat_`.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::PersonalAccessToken => "slath_pat_",
            Self::BotToken => "slath_bot_",
            Self::TaskToken => "slath_tsk_",
            Self::RunnerToken => "slath_rnr_",
        }
    }

    /// A human-readable name, for scanner findings and messages.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::PersonalAccessToken => "Slath personal access token",
            Self::BotToken => "Slath bot token",
            Self::TaskToken => "Slath task token",
            Self::RunnerToken => "Slath runner token",
        }
    }

    fn from_tag(tag: &[u8]) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|k| &k.prefix().as_bytes()[6..9] == tag)
    }
}

/// Length of the random part (43 × log2(62) ≥ 256 bits).
pub const RANDOM_LEN: usize = 43;
/// Length of the checksum (62⁶ > 2³²).
pub const CHECKSUM_LEN: usize = 6;
/// Length of the prefix, `slath_xxx_`.
pub const PREFIX_LEN: usize = 10;
/// Total length of a credential.
pub const TOKEN_LEN: usize = PREFIX_LEN + RANDOM_LEN + CHECKSUM_LEN;

/// The base62 alphabet used for the random part and the checksum.
pub const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// CRC-32 (IEEE 802.3, the zlib polynomial).
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn checksum(body: &[u8]) -> [u8; CHECKSUM_LEN] {
    let mut n = crc32(body);
    let mut out = [b'0'; CHECKSUM_LEN];
    for slot in out.iter_mut().rev() {
        *slot = BASE62[(n % 62) as usize];
        n /= 62;
    }
    out
}

/// Builds a credential from its kind and 43 base62 characters. Returns
/// `None` if `random` has the wrong length or characters.
#[must_use]
pub fn assemble(kind: CredentialKind, random: &str) -> Option<String> {
    if random.len() != RANDOM_LEN || !random.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let mut out = String::with_capacity(TOKEN_LEN);
    out.push_str(kind.prefix());
    out.push_str(random);
    let sum = checksum(out.as_bytes());
    out.extend(sum.iter().map(|&b| char::from(b)));
    Some(out)
}

/// The kind of `s` if it is exactly one well-formed credential with a
/// correct checksum.
#[must_use]
pub fn parse(s: &str) -> Option<CredentialKind> {
    let b = s.as_bytes();
    if b.len() != TOKEN_LEN || !b.starts_with(b"slath_") || b[9] != b'_' {
        return None;
    }
    let kind = CredentialKind::from_tag(&b[6..9])?;
    let (body, sum) = b.split_at(PREFIX_LEN + RANDOM_LEN);
    if !body[PREFIX_LEN..].iter().all(u8::is_ascii_alphanumeric) {
        return None;
    }
    (checksum(body) == sum).then_some(kind)
}

/// The first 8 random characters, stored for display (`slath_pat_AbCd1234…`).
#[must_use]
pub fn display_prefix(s: &str) -> &str {
    s.get(PREFIX_LEN..PREFIX_LEN + 8).unwrap_or_default()
}

/// A credential found in text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Found {
    pub kind: CredentialKind,
    /// Byte offset of the first character.
    pub start: usize,
}

/// Every valid credential in `text`, in order, in time linear in its length.
/// A match must not be part of a longer word: the characters around it may
/// not be ASCII letters, digits, or `_`.
#[must_use]
pub fn find_all(text: &[u8]) -> Vec<Found> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(pos) = find_from(text, from) {
        let end = pos + TOKEN_LEN;
        let candidate = text.get(pos..end);
        let before_ok = pos == 0 || !is_word(text[pos - 1]);
        let after_ok = text.get(end).is_none_or(|&c| !is_word(c));
        if let (Some(c), true, true) = (candidate, before_ok, after_ok)
            && let Some(kind) = std::str::from_utf8(c).ok().and_then(parse)
        {
            out.push(Found { kind, start: pos });
            from = end;
        } else {
            from = pos + 1;
        }
    }
    out
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn find_from(text: &[u8], from: usize) -> Option<usize> {
    static NEEDLE: std::sync::OnceLock<regex::bytes::Regex> = std::sync::OnceLock::new();
    let re = NEEDLE.get_or_init(|| {
        regex::bytes::Regex::new("slath_").unwrap_or_else(|_| unreachable!("a literal compiles"))
    });
    re.find_at(text, from).map(|m| m.start())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const R: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefg";

    #[test]
    fn crc32_matches_the_reference() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn round_trip_for_every_kind() {
        for kind in CredentialKind::ALL {
            let t = assemble(kind, R).unwrap();
            assert_eq!(t.len(), TOKEN_LEN);
            assert!(t.starts_with(kind.prefix()));
            assert_eq!(parse(&t), Some(kind));
            assert_eq!(display_prefix(&t), "01234567");
        }
    }

    #[test]
    fn rejects_bad_tokens() {
        let t = assemble(CredentialKind::PersonalAccessToken, R).unwrap();
        // One changed random character.
        let mut bad = t.clone().into_bytes();
        bad[20] = if bad[20] == b'a' { b'b' } else { b'a' };
        assert_eq!(parse(std::str::from_utf8(&bad).unwrap()), None);
        // Wrong kind with the same body.
        assert_eq!(parse(&t.replacen("_pat_", "_bot_", 1)), None);
        assert_eq!(parse(&t.replacen("_pat_", "_xyz_", 1)), None);
        assert_eq!(parse(&t[..t.len() - 1]), None);
        assert_eq!(parse(&format!("{t}0")), None);
        assert_eq!(parse(&format!("slath_pat_{}", "-".repeat(49))), None);
        assert_eq!(assemble(CredentialKind::BotToken, "short"), None);
        assert_eq!(assemble(CredentialKind::BotToken, &"-".repeat(43)), None);
    }

    #[test]
    fn finds_tokens_in_text() {
        let a = assemble(CredentialKind::PersonalAccessToken, R).unwrap();
        let b = assemble(CredentialKind::RunnerToken, R).unwrap();
        let text = format!("token = \"{a}\"\nslath_ nothing slath_pat_x\n{b}");
        let found = find_all(text.as_bytes());
        assert_eq!(
            found,
            vec![
                Found {
                    kind: CredentialKind::PersonalAccessToken,
                    start: 9
                },
                Found {
                    kind: CredentialKind::RunnerToken,
                    start: text.len() - TOKEN_LEN
                },
            ]
        );
        // Part of a longer word: no match.
        assert!(find_all(format!("x{a}").as_bytes()).is_empty());
        assert!(find_all(format!("{a}x").as_bytes()).is_empty());
        // Overlapping prefixes don't hide a real token.
        assert_eq!(find_all(format!("slath_slath_ {a}").as_bytes()).len(), 1);
    }

    proptest! {
        #[test]
        fn random_bodies_round_trip(body in "[0-9A-Za-z]{43}") {
            let t = assemble(CredentialKind::BotToken, &body).unwrap();
            prop_assert_eq!(parse(&t), Some(CredentialKind::BotToken));
            prop_assert_eq!(find_all(t.as_bytes()).len(), 1);
        }

        #[test]
        fn arbitrary_text_never_panics(s in ".{0,200}") {
            let _ = find_all(s.as_bytes());
            let _ = parse(&s);
        }
    }
}
