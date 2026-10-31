// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! A raw commit parser that preserves every header in order.
//!
//! Server-side rebases must keep jj's `change-id` and any unknown headers,
//! so commits are parsed into an ordered header list that serializes
//! back to the exact original bytes.

/// One commit header. Multi-line values (e.g. `gpgsig`) have their
/// continuation lines joined with `\n`, without the leading space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub name: Vec<u8>,
    /// `None` for a header line without a space (no value at all).
    pub value: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCommit {
    pub headers: Vec<Header>,
    /// Everything after the blank line that ends the headers.
    pub message: Vec<u8>,
    /// False for (unusual) commits without the blank separator line.
    has_separator: bool,
}

impl RawCommit {
    pub fn parse(data: impl AsRef<[u8]>) -> Result<Self, &'static str> {
        let data = data.as_ref();
        let mut headers: Vec<Header> = Vec::new();
        let mut rest = data;
        let mut has_separator = false;
        while !rest.is_empty() {
            let (line, next) = match rest.iter().position(|&b| b == b'\n') {
                Some(i) => (&rest[..i], &rest[i + 1..]),
                None => return Err("commit header without newline"),
            };
            rest = next;
            if line.is_empty() {
                has_separator = true;
                break;
            }
            if let Some(cont) = line.strip_prefix(b" ") {
                let value = headers
                    .last_mut()
                    .and_then(|h| h.value.as_mut())
                    .ok_or("continuation line without a header value")?;
                value.push(b'\n');
                value.extend_from_slice(cont);
                continue;
            }
            let header = match line.iter().position(|&b| b == b' ') {
                Some(i) => Header {
                    name: line[..i].to_vec(),
                    value: Some(line[i + 1..].to_vec()),
                },
                None => Header {
                    name: line.to_vec(),
                    value: None,
                },
            };
            headers.push(header);
        }
        let commit = Self {
            headers,
            message: rest.to_vec(),
            has_separator,
        };
        if commit.header(b"tree").is_none() {
            return Err("commit without tree header");
        }
        Ok(commit)
    }

    /// Serializes back to the canonical object bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for h in &self.headers {
            out.extend_from_slice(&h.name);
            if let Some(v) = &h.value {
                out.push(b' ');
                for (i, line) in v.split(|&b| b == b'\n').enumerate() {
                    if i > 0 {
                        out.extend_from_slice(b"\n ");
                    }
                    out.extend_from_slice(line);
                }
            }
            out.push(b'\n');
        }
        if self.has_separator {
            out.push(b'\n');
        }
        out.extend_from_slice(&self.message);
        out
    }

    /// The first header with this name.
    #[must_use]
    pub fn header(&self, name: &[u8]) -> Option<&[u8]> {
        self.headers
            .iter()
            .find(|h| h.name == name)
            .and_then(|h| h.value.as_deref())
    }

    #[must_use]
    pub fn tree(&self) -> Option<&[u8]> {
        self.header(b"tree")
    }

    pub fn parents(&self) -> impl Iterator<Item = &[u8]> {
        self.headers
            .iter()
            .filter(|h| h.name == b"parent")
            .filter_map(|h| h.value.as_deref())
    }

    /// jj's change ID: the `change-id` header, valid iff it matches `^[k-z]{32}$`.
    #[must_use]
    pub fn change_id(&self) -> Option<&str> {
        let v = self.header(b"change-id")?;
        (v.len() == 32 && v.iter().all(|b| (b'k'..=b'z').contains(b)))
            .then(|| std::str::from_utf8(v).ok())
            .flatten()
    }

    /// True if the commit has a `jj:trees` header (a jj conflict commit).
    #[must_use]
    pub fn has_jj_trees_header(&self) -> bool {
        self.headers.iter().any(|h| h.name == b"jj:trees")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    #[test]
    fn round_trips_and_exposes_headers() {
        let raw = format!(
            "tree {TREE}\nparent {TREE}\nauthor A <a@x> 1 +0000\ncommitter C <c@x> 1 +0000\n\
             change-id {}\nx-unknown first\n second\n\nmsg\n",
            "k".repeat(32)
        );
        let c = RawCommit::parse(raw.as_bytes()).unwrap();
        assert_eq!(c.to_bytes(), raw.as_bytes());
        assert_eq!(c.change_id(), Some("k".repeat(32).as_str()));
        assert_eq!(c.parents().count(), 1);
        assert_eq!(c.header(b"x-unknown"), Some(&b"first\nsecond"[..]));
        assert!(!c.has_jj_trees_header());
    }

    #[test]
    fn invalid_change_ids_are_ignored() {
        for bad in ["abc", &"a".repeat(32), &"k".repeat(31), &"K".repeat(32)] {
            let raw = format!("tree {TREE}\nchange-id {bad}\n\n");
            assert_eq!(RawCommit::parse(raw.as_bytes()).unwrap().change_id(), None);
        }
    }

    #[test]
    fn edge_cases_round_trip() {
        for raw in [
            format!("tree {TREE}\n"),
            format!("tree {TREE}\nnovalue\n\n"),
            format!("tree {TREE}\ngpgsig -----BEGIN-----\n \n line\n -----END-----\n\n\n\nm"),
            format!("tree {TREE}\njj:trees a b\n\n"),
        ] {
            let c = RawCommit::parse(raw.as_bytes()).unwrap();
            assert_eq!(c.to_bytes(), raw.as_bytes(), "{raw:?}");
        }
        assert!(RawCommit::parse(b"parent x\n\n").is_err());
        assert!(RawCommit::parse(b" cont\n").is_err());
    }
}
