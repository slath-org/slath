// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Change identity inside a merge request: the aliases a commit carries,
//! jj's ID for commits without one, and matching the commits of a new
//! revision to the changes of the merge request.
//!
//! Matching runs in order, each step one-to-one: an alias naming a change,
//! an identical patch, a content pairing with `git range-diff`'s cost
//! model, otherwise a new change. Conflicting or duplicate aliases are
//! never resolved here; they come back as unresolved.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// jj's alphabet for change IDs: hex digit `0` is `z`, `f` is `k`.
const JJ_DIGITS: &[u8; 16] = b"zyxwvutsrqponmlk";

/// What an alias is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AliasKind {
    /// A jj change ID (`^[k-z]{32}$`): the header, jj's trailer, or the
    /// predicted ID of a commit without a header.
    Jj,
    /// A Gerrit-style `Change-Id: I<40 hex>` trailer.
    Gerrit,
}

impl AliasKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Jj => "jj",
            Self::Gerrit => "gerrit",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "jj" => Some(Self::Jj),
            "gerrit" => Some(Self::Gerrit),
            _ => None,
        }
    }
}

/// One alias of a commit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Alias {
    pub kind: AliasKind,
    pub value: String,
}

impl Alias {
    fn jj(value: String) -> Self {
        Self {
            kind: AliasKind::Jj,
            value,
        }
    }
}

/// Whether `s` is a jj change ID.
#[must_use]
pub fn is_jj_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| (b'k'..=b'z').contains(&b))
}

fn hex_to_jj(hex: &str) -> Option<String> {
    hex.bytes()
        .map(|b| {
            char::from(b)
                .to_digit(16)
                .filter(|_| !b.is_ascii_uppercase())
                .map(|d| char::from(JJ_DIGITS[d as usize]))
        })
        .collect()
}

/// A change ID in jj's format from 16 bytes (for new IDs Slath makes up,
/// such as a squash of several changes).
#[must_use]
pub fn id_from_bytes(bytes: &[u8; 16]) -> String {
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    hex_to_jj(&hex).unwrap_or_default()
}

/// jj's change ID for a commit without a `change-id` header: bytes 4..20
/// of the commit ID in reverse order, each byte's bits reversed, in jj's
/// alphabet. `None` for anything but a SHA-1 commit ID in lower-case hex.
#[must_use]
pub fn predict(oid: &str) -> Option<String> {
    if oid.len() != 40
        || !oid
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let bytes: Vec<u8> = (0..20)
        .map(|i| u8::from_str_radix(&oid[2 * i..2 * i + 2], 16).unwrap_or(0))
        .collect();
    let id: String = bytes[4..20]
        .iter()
        .rev()
        .map(|b| format!("{:02x}", b.reverse_bits()))
        .collect();
    hex_to_jj(&id)
}

/// The alias a `Change-Id` trailer value names: jj's form
/// (`I<32 hex>6a6a6964`) normalizes to the jj ID, Gerrit's
/// (`I<40 hex>`) stays as it is. Anything else is ignored.
#[must_use]
pub fn trailer_alias(value: &str) -> Option<Alias> {
    let hex = value.strip_prefix('I')?;
    let lower = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    if hex.len() == 40 && lower(hex) {
        if let Some(jj) = hex.strip_suffix("6a6a6964") {
            // 32 hex digits plus jj's marker: also a valid Gerrit ID, but
            // jj wrote it.
            return hex_to_jj(jj).map(Alias::jj);
        }
        return Some(Alias {
            kind: AliasKind::Gerrit,
            value: value.to_owned(),
        });
    }
    None
}

/// The aliases of a commit, in precedence order for display: the header,
/// the trailer, and for a commit without a header its predicted jj ID.
/// `header` must already be the single valid header (see
/// `RawCommit::change_id`). Duplicates are left out.
#[must_use]
pub fn aliases(header: Option<&str>, trailer: Option<&str>, oid: &str) -> Vec<Alias> {
    let mut out = Vec::new();
    let header = header.filter(|h| is_jj_id(h));
    if let Some(h) = header {
        out.push(Alias::jj(h.to_owned()));
    }
    if let Some(t) = trailer.and_then(trailer_alias)
        && !out.contains(&t)
    {
        out.push(t);
    }
    if header.is_none()
        && let Some(p) = predict(oid).map(Alias::jj)
        && !out.contains(&p)
    {
        out.push(p);
    }
    out
}

/// The display ID of a new change: the header, else jj's trailer, else
/// the predicted ID.
#[must_use]
pub fn display_id(aliases: &[Alias], oid: &str) -> Option<String> {
    aliases
        .iter()
        .find(|a| a.kind == AliasKind::Jj)
        .map(|a| a.value.clone())
        .or_else(|| predict(oid))
}

/// A commit of the new revision, bottom of the stack first.
#[derive(Debug, Clone, Default)]
pub struct Commit {
    pub aliases: Vec<Alias>,
    /// The alias from the trailer, if any (offered first in conflicts).
    pub trailer: Option<Alias>,
    /// `git patch-id --stable`.
    pub patch_id: Option<String>,
    /// The patch's lines (`+`/`-`/context lines of the diff), hashed.
    pub patch: Vec<u64>,
}

/// A change with a version in the previous revision, and its latest patch.
#[derive(Debug, Clone)]
pub struct Previous<K> {
    pub change: K,
    pub patch_id: Option<String>,
    pub patch: Vec<u64>,
}

/// Why a commit is unresolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conflict {
    /// Its aliases name different changes.
    AliasesConflict,
    /// Another commit of the revision names the same change or carries the
    /// same alias.
    DuplicateId,
}

impl Conflict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AliasesConflict => "aliases-conflict",
            Self::DuplicateId => "duplicate-id",
        }
    }
}

/// How a commit was bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding<K> {
    /// An alias named the change.
    Alias(K),
    /// The patch is identical to the change's latest version.
    Patch(K),
    /// Paired by content.
    Inferred(K),
    /// Blocks merge until someone picks a change; candidates first.
    Unresolved {
        reason: Conflict,
        candidates: Vec<K>,
    },
    /// A new change; `candidates` are equally good content partners it
    /// "may continue".
    New { candidates: Vec<K> },
}

impl<K> Binding<K> {
    /// The change it continues, if any.
    pub const fn change(&self) -> Option<&K> {
        match self {
            Self::Alias(k) | Self::Patch(k) | Self::Inferred(k) => Some(k),
            _ => None,
        }
    }
}

/// The share of a patch's size below which pairing beats creating it anew,
/// in percent (`git range-diff --creation-factor`, default 60).
pub const CREATION_FACTOR: u64 = 60;

/// At most this many (change, commit) pairs are compared by content; with
/// more, the remaining commits become new changes.
pub const MAX_PAIRINGS: usize = 10_000;

/// Matches the commits of a new revision to changes (spec: change identity
/// assignment). `bound` maps the merge request's aliases to their changes;
/// `previous` are the changes of the previous revision.
pub fn assign<K>(
    commits: &[Commit],
    bound: &HashMap<Alias, K>,
    previous: &[Previous<K>],
) -> Vec<Binding<K>>
where
    K: Clone + Eq + Hash,
{
    let n = commits.len();
    let mut out: Vec<Option<Binding<K>>> = vec![None; n];
    // 1. Aliases. The changes each commit names, trailer first.
    let named: Vec<Vec<K>> = commits
        .iter()
        .map(|c| {
            let mut ks: Vec<K> = Vec::new();
            let order = c
                .trailer
                .iter()
                .chain(c.aliases.iter().filter(|a| Some(*a) != c.trailer.as_ref()));
            for a in order {
                if let Some(k) = bound.get(a)
                    && !ks.contains(k)
                {
                    ks.push(k.clone());
                }
            }
            ks
        })
        .collect();
    let mut alias_users: HashMap<&Alias, Vec<usize>> = HashMap::new();
    for (i, c) in commits.iter().enumerate() {
        for a in &c.aliases {
            alias_users.entry(a).or_default().push(i);
        }
    }
    let mut change_users: HashMap<&K, Vec<usize>> = HashMap::new();
    for (i, ks) in named.iter().enumerate() {
        for k in ks {
            change_users.entry(k).or_default().push(i);
        }
    }
    let mut taken: HashSet<K> = HashSet::new();
    for i in 0..n {
        let dup = commits[i]
            .aliases
            .iter()
            .any(|a| alias_users.get(a).is_some_and(|u| u.len() > 1))
            || named[i]
                .iter()
                .any(|k| change_users.get(k).is_some_and(|u| u.len() > 1));
        if dup {
            out[i] = Some(Binding::Unresolved {
                reason: Conflict::DuplicateId,
                candidates: named[i].clone(),
            });
        } else if named[i].len() > 1 {
            out[i] = Some(Binding::Unresolved {
                reason: Conflict::AliasesConflict,
                candidates: named[i].clone(),
            });
        } else if let Some(k) = named[i].first() {
            out[i] = Some(Binding::Alias(k.clone()));
        }
        // Changes a commit names are spoken for, resolved or not.
        taken.extend(named[i].iter().cloned());
    }

    // 2. Identical patches, one-to-one.
    let open: Vec<usize> = (0..n).filter(|i| out[*i].is_none()).collect();
    let mut by_patch: HashMap<&str, Vec<usize>> = HashMap::new();
    for &i in &open {
        if let Some(p) = commits[i].patch_id.as_deref() {
            by_patch.entry(p).or_default().push(i);
        }
    }
    let mut prev_by_patch: HashMap<&str, Vec<&Previous<K>>> = HashMap::new();
    for p in previous.iter().filter(|p| !taken.contains(&p.change)) {
        if let Some(id) = p.patch_id.as_deref() {
            prev_by_patch.entry(id).or_default().push(p);
        }
    }
    for (pid, users) in &by_patch {
        if let ([i], Some([p])) = (users.as_slice(), prev_by_patch.get(pid).map(Vec::as_slice)) {
            out[*i] = Some(Binding::Patch(p.change.clone()));
            taken.insert(p.change.clone());
        }
    }

    // 3. Content pairing with range-diff's costs.
    let rest: Vec<usize> = (0..n).filter(|i| out[*i].is_none()).collect();
    let olds: Vec<&Previous<K>> = previous
        .iter()
        .filter(|p| !taken.contains(&p.change))
        .collect();
    if !rest.is_empty() && !olds.is_empty() && rest.len() * olds.len() <= MAX_PAIRINGS {
        let create = |len: usize| (len as u64) * CREATION_FACTOR / 100;
        // cost[o][c]: None when pairing is no cheaper than creating both anew.
        let cost: Vec<Vec<Option<u64>>> = olds
            .iter()
            .map(|o| {
                rest.iter()
                    .map(|&c| {
                        let limit = create(o.patch.len()) + create(commits[c].patch.len());
                        distance(&o.patch, &commits[c].patch, limit).filter(|d| *d < limit)
                    })
                    .collect()
            })
            .collect();
        for (oi, ci) in pair(&cost, &olds, &rest, commits, create) {
            let d = cost[oi][ci];
            let rivals: Vec<K> = (0..olds.len())
                .filter(|&o| o != oi && cost[o][ci].is_some() && cost[o][ci] == d)
                .map(|o| olds[o].change.clone())
                .collect();
            out[rest[ci]] = Some(if rivals.is_empty() {
                Binding::Inferred(olds[oi].change.clone())
            } else {
                let mut candidates = vec![olds[oi].change.clone()];
                candidates.extend(rivals);
                Binding::New { candidates }
            });
        }
    }

    // 4. New changes.
    out.into_iter()
        .map(|b| {
            b.unwrap_or(Binding::New {
                candidates: Vec::new(),
            })
        })
        .collect()
}

/// Minimum-cost pairing of old changes and new commits, where leaving
/// either side unpaired costs creating it anew (range-diff's model).
/// Returns (old index, commit index) pairs.
fn pair<K>(
    cost: &[Vec<Option<u64>>],
    olds: &[&Previous<K>],
    rest: &[usize],
    commits: &[Commit],
    create: impl Fn(usize) -> u64,
) -> Vec<(usize, usize)> {
    let (no, nc) = (olds.len(), rest.len());
    let size = no + nc;
    // Rows: old changes, then a "created" slot per commit. Columns: new
    // commits, then a "removed" slot per old change.
    let big = u64::MAX / 4;
    let mut m = vec![vec![0u64; size]; size];
    for (r, row) in m.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            *cell = match (r < no, c < nc) {
                (true, true) => cost[r][c].unwrap_or(big),
                (true, false) => create(olds[r].patch.len()),
                (false, true) => create(commits[rest[c]].patch.len()),
                (false, false) => 0,
            };
        }
    }
    hungarian(&m)
        .into_iter()
        .enumerate()
        .filter(|&(r, c)| r < no && c < nc && cost[r][c].is_some())
        .collect()
}

/// The column assigned to each row in a minimum-cost assignment of a
/// square matrix (Kuhn–Munkres, O(n³)).
fn hungarian(a: &[Vec<u64>]) -> Vec<usize> {
    let n = a.len();
    let cost = |i: usize, j: usize| i128::from(a[i - 1][j - 1]);
    let inf = i128::MAX / 4;
    let (mut u, mut v) = (vec![0i128; n + 1], vec![0i128; n + 1]);
    let (mut p, mut way) = (vec![0usize; n + 1], vec![0usize; n + 1]);
    for i in 1..=n {
        p[0] = i;
        let mut j0 = 0;
        let mut minv = vec![inf; n + 1];
        let mut used = vec![false; n + 1];
        loop {
            used[j0] = true;
            let i0 = p[j0];
            let mut delta = inf;
            let mut j1 = 0;
            for j in 1..=n {
                if !used[j] {
                    let cur = cost(i0, j) - u[i0] - v[j];
                    if cur < minv[j] {
                        minv[j] = cur;
                        way[j] = j0;
                    }
                    if minv[j] < delta {
                        delta = minv[j];
                        j1 = j;
                    }
                }
            }
            for j in 0..=n {
                if used[j] {
                    u[p[j]] += delta;
                    v[j] -= delta;
                } else {
                    minv[j] -= delta;
                }
            }
            j0 = j1;
            if p[j0] == 0 {
                break;
            }
        }
        loop {
            let j1 = way[j0];
            p[j0] = p[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    let mut row = vec![0usize; n];
    for j in 1..=n {
        if p[j] > 0 {
            row[p[j] - 1] = j - 1;
        }
    }
    row
}

/// The number of lines to add and remove to turn `a` into `b` (Myers),
/// or `None` if it's more than `max`.
#[must_use]
pub fn distance(a: &[u64], b: &[u64], max: u64) -> Option<u64> {
    let (n, m) = (a.len() as i64, b.len() as i64);
    let max = i64::try_from(max).unwrap_or(i64::MAX).min(n + m);
    let off = max + 1;
    let mut v = vec![0i64; usize::try_from(2 * max + 3).ok()?];
    for d in 0..=max {
        let mut k = -d;
        while k <= d {
            let idx = |k: i64| usize::try_from(k + off).unwrap_or(0);
            let mut x = if k == -d || (k != d && v[idx(k - 1)] < v[idx(k + 1)]) {
                v[idx(k + 1)]
            } else {
                v[idx(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx(k)] = x;
            if x >= n && y >= m {
                return u64::try_from(d).ok();
            }
            k += 2;
        }
    }
    None
}

/// Hashes the lines of a patch for [`Commit::patch`]: only the lines that
/// make up the change (`+`, `-`, and context), not file headers or hunk
/// positions, so the same change at another line number costs nothing.
#[must_use]
pub fn patch_lines(patch: &[u8]) -> Vec<u64> {
    use std::hash::{DefaultHasher, Hasher};
    patch
        .split(|b| *b == b'\n')
        .filter(|l| {
            !(l.starts_with(b"+++ ")
                || l.starts_with(b"--- ")
                || l.starts_with(b"@@")
                || l.starts_with(b"diff --git ")
                || l.starts_with(b"index "))
        })
        .filter(|l| matches!(l.first(), Some(b'+' | b'-' | b' ')))
        .map(|l| {
            let mut h = DefaultHasher::new();
            h.write(l);
            h.finish()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "38c720d16e19d94659b8762526416311994bc62c";

    fn jj(v: &str) -> Alias {
        Alias::jj(v.to_owned())
    }

    fn lines(text: &str) -> Vec<u64> {
        patch_lines(text.as_bytes())
    }

    fn commit(aliases: Vec<Alias>, patch_id: &str, patch: &str) -> Commit {
        Commit {
            aliases,
            trailer: None,
            patch_id: Some(patch_id.to_owned()),
            patch: lines(patch),
        }
    }

    fn prev(k: u32, patch_id: &str, patch: &str) -> Previous<u32> {
        Previous {
            change: k,
            patch_id: Some(patch_id.to_owned()),
            patch: lines(patch),
        }
    }

    // Values checked against jj 0.46.0 (`jj log -T change_id` in a colocated
    // repository, and `format_gerrit_change_id_trailer`).
    #[test]
    fn prediction_matches_jj() {
        assert_eq!(predict(OID).unwrap(), "wvtwmxqqrrntrxtvpvtlymqptxqoqrst");
        assert_eq!(
            predict("987f9987e8a73d924dab36de064fe91575263750").unwrap(),
            "zplntvplprqskxtzsotnmuoxvqonluys"
        );
        assert_eq!(
            predict("42cdba6b404887eef9eca05e5127ff58fcc4bc18").unwrap(),
            "yrwmxwwkypkklvrpspzuwsqksslyyxzx"
        );
        assert_eq!(
            predict("0000000000000000000000000000000000000000").unwrap(),
            "z".repeat(32)
        );
        for bad in [
            "",
            "38C720D16E19D94659B8762526416311994BC62C",
            &OID[1..],
            "x".repeat(40).as_str(),
        ] {
            assert_eq!(predict(bad), None, "{bad}");
        }
    }

    #[test]
    fn trailers_normalize() {
        assert_eq!(
            trailer_alias("I3463d29988c68264a46e1d9a629b98766a6a6964"),
            Some(jj("wvtwmxqqrrntrxtvpvtlymqptxqoqrst"))
        );
        let g = "I0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            trailer_alias(g),
            Some(Alias {
                kind: AliasKind::Gerrit,
                value: g.to_owned()
            })
        );
        for bad in [
            "",
            "I123",
            "i0123456789abcdef0123456789abcdef01234567",
            "I0123456789ABCDEF0123456789abcdef01234567",
        ] {
            assert_eq!(trailer_alias(bad), None, "{bad}");
        }
    }

    #[test]
    fn aliases_of_a_commit() {
        let h = "k".repeat(32);
        // Header only: no prediction.
        assert_eq!(aliases(Some(&h), None, OID), vec![jj(&h)]);
        // Trailer only: the trailer and the prediction.
        let g = "I0123456789abcdef0123456789abcdef01234567";
        let a = aliases(None, Some(g), OID);
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].kind, AliasKind::Gerrit);
        assert_eq!(a[1], jj("wvtwmxqqrrntrxtvpvtlymqptxqoqrst"));
        // jj's trailer for the header it names: one alias.
        let t = "I3463d29988c68264a46e1d9a629b98766a6a6964";
        let h2 = "wvtwmxqqrrntrxtvpvtlymqptxqoqrst";
        assert_eq!(aliases(Some(h2), Some(t), OID), vec![jj(h2)]);
        // A malformed header counts as none.
        assert_eq!(aliases(Some("short"), None, OID), vec![jj(h2)]);
        assert_eq!(display_id(&aliases(None, Some(g), OID), OID).unwrap(), h2);
    }

    /// A patch of ten added lines `+<tag> <i>`, with `edit` replacing one.
    fn body(tag: &str, edit: Option<&str>) -> String {
        (0..10)
            .map(|i| match edit {
                Some(e) if i == 5 => format!("+{e}\n"),
                _ => format!("+{tag} {i}\n"),
            })
            .collect()
    }

    const P1: &str =
        "+one 0\n+one 1\n+one 2\n+one 3\n+one 4\n+one 5\n+one 6\n+one 7\n+one 8\n+one 9\n";
    const P2: &str =
        "+two 0\n+two 1\n+two 2\n+two 3\n+two 4\n+two 5\n+two 6\n+two 7\n+two 8\n+two 9\n";
    const P3: &str =
        "+six 0\n+six 1\n+six 2\n+six 3\n+six 4\n+six 5\n+six 6\n+six 7\n+six 8\n+six 9\n";

    #[test]
    fn header_only_continues_the_change() {
        let bound = HashMap::from([(jj(&"k".repeat(32)), 1u32)]);
        let c = commit(vec![jj(&"k".repeat(32))], "p9", P3);
        let got = assign(&[c], &bound, &[prev(1, "p1", P1)]);
        assert_eq!(got, vec![Binding::Alias(1)]);
    }

    #[test]
    fn trailer_only_and_jj_trailer() {
        let g = Alias {
            kind: AliasKind::Gerrit,
            value: "I0123456789abcdef0123456789abcdef01234567".into(),
        };
        let bound = HashMap::from([
            (g.clone(), 1u32),
            (jj("wvtwmxqqrrntrxtvpvtlymqptxqoqrst"), 2),
        ]);
        let mut c = commit(vec![g.clone()], "p9", P3);
        c.trailer = Some(g);
        let mut d = commit(
            aliases(
                None,
                Some("I3463d29988c68264a46e1d9a629b98766a6a6964"),
                &"1".repeat(40),
            ),
            "p8",
            P2,
        );
        d.trailer = trailer_alias("I3463d29988c68264a46e1d9a629b98766a6a6964");
        let got = assign(&[c, d], &bound, &[]);
        assert_eq!(got, vec![Binding::Alias(1), Binding::Alias(2)]);
    }

    #[test]
    fn header_and_trailer_naming_one_or_two_changes() {
        let h = jj(&"k".repeat(32));
        let g = Alias {
            kind: AliasKind::Gerrit,
            value: "I0123456789abcdef0123456789abcdef01234567".into(),
        };
        let mut c = commit(vec![h.clone(), g.clone()], "p", P1);
        c.trailer = Some(g.clone());
        // Both name change 1.
        let one = HashMap::from([(h.clone(), 1u32), (g.clone(), 1)]);
        assert_eq!(
            assign(std::slice::from_ref(&c), &one, &[]),
            vec![Binding::Alias(1)]
        );
        // Different changes: unresolved, the trailer's change first.
        let two = HashMap::from([(h, 1u32), (g, 2)]);
        assert_eq!(
            assign(&[c], &two, &[]),
            vec![Binding::Unresolved {
                reason: Conflict::AliasesConflict,
                candidates: vec![2, 1]
            }]
        );
    }

    #[test]
    fn duplicate_ids_are_unresolved() {
        let h = jj(&"k".repeat(32));
        let bound = HashMap::from([(h.clone(), 1u32)]);
        let a = commit(vec![h.clone()], "a", P1);
        let b = commit(vec![h], "b", P2);
        let got = assign(&[a, b], &bound, &[]);
        for g in got {
            assert_eq!(
                g,
                Binding::Unresolved {
                    reason: Conflict::DuplicateId,
                    candidates: vec![1]
                }
            );
        }
        // Also when the shared header names no change yet.
        let x = jj(&"x".repeat(32));
        let got = assign(
            &[commit(vec![x.clone()], "a", P1), commit(vec![x], "b", P2)],
            &HashMap::<Alias, u32>::new(),
            &[],
        );
        assert!(got.iter().all(|g| matches!(
            g,
            Binding::Unresolved {
                reason: Conflict::DuplicateId,
                ..
            }
        )));
    }

    #[test]
    fn no_alias_and_identical_patch() {
        let bound = HashMap::<Alias, u32>::new();
        let previous = [prev(1, "p1", P1), prev(2, "p2", P2)];
        // A plain git rebase: new commit IDs, same patches.
        let got = assign(
            &[
                commit(vec![jj(&"y".repeat(32))], "p1", P1),
                commit(vec![jj(&"x".repeat(32))], "p2", P2),
            ],
            &bound,
            &previous,
        );
        assert_eq!(got, vec![Binding::Patch(1), Binding::Patch(2)]);
        // No alias, nothing alike: a new change.
        let got = assign(&[commit(vec![], "p3", P3)], &bound, &previous);
        assert_eq!(got, vec![Binding::New { candidates: vec![] }]);
    }

    #[test]
    fn identical_patch_only_one_to_one() {
        let previous = [prev(1, "p1", P1), prev(2, "p1", P1)];
        let got = assign(&[commit(vec![], "p1", P1)], &HashMap::new(), &previous);
        // Two changes with that patch: not an identical match; equally good
        // content partners make a new change with both as candidates.
        let Binding::New { mut candidates } = got[0].clone() else {
            panic!("{got:?}");
        };
        candidates.sort_unstable();
        assert_eq!(candidates, vec![1, 2]);
    }

    #[test]
    fn inferred_pairing() {
        let previous = [prev(1, "p1", P1), prev(2, "p2", P2)];
        let p1b = body("one", Some("edited"));
        assert_eq!(body("one", None), P1);
        let got = assign(&[commit(vec![], "p1b", &p1b)], &HashMap::new(), &previous);
        assert_eq!(got, vec![Binding::Inferred(1)]);
        // Changes taken by an alias aren't paired by content.
        let h = jj(&"k".repeat(32));
        let bound = HashMap::from([(h.clone(), 1u32)]);
        let got = assign(
            &[commit(vec![h], "p3", P3), commit(vec![], "p1b", &p1b)],
            &bound,
            &previous,
        );
        assert_eq!(
            got,
            vec![Binding::Alias(1), Binding::New { candidates: vec![] }]
        );
    }

    #[test]
    fn ambiguous_pairing_makes_a_new_change() {
        let a = body("shared", Some("left"));
        let b = body("shared", Some("right"));
        let previous = [prev(1, "pa", &a), prev(2, "pb", &b)];
        let c = body("shared", Some("middle"));
        let got = assign(&[commit(vec![], "pc", &c)], &HashMap::new(), &previous);
        let Binding::New { candidates } = &got[0] else {
            panic!("{got:?}");
        };
        let mut c = candidates.clone();
        c.sort_unstable();
        assert_eq!(c, vec![1, 2]);
    }

    #[test]
    fn message_similarity_alone_never_matches() {
        // Patches with nothing in common don't pair, whatever the messages
        // say (messages aren't compared at all).
        let previous = [prev(1, "p1", P2)];
        let got = assign(&[commit(vec![], "p3", P3)], &HashMap::new(), &previous);
        assert_eq!(got, vec![Binding::New { candidates: vec![] }]);
    }

    #[test]
    fn myers_distance() {
        let a = lines("+a\n+b\n+c\n");
        let b = lines("+a\n+x\n+c\n");
        assert_eq!(distance(&a, &b, 10), Some(2));
        assert_eq!(distance(&a, &a, 0), Some(0));
        assert_eq!(distance(&a, &b, 1), None);
        assert_eq!(distance(&[], &b, 10), Some(3));
    }

    #[test]
    fn hungarian_minimizes() {
        let m = vec![vec![4, 1, 3], vec![2, 0, 5], vec![3, 2, 2]];
        let row = hungarian(&m);
        let total: u64 = row.iter().enumerate().map(|(r, c)| m[r][*c]).sum();
        assert_eq!(total, 5);
    }

    #[test]
    fn patch_lines_skip_positions() {
        let a = patch_lines(
            b"diff --git a/x b/x\nindex 1..2\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let b = patch_lines(
            b"diff --git a/x b/x\nindex 3..4\n--- a/x\n+++ b/x\n@@ -9 +9 @@\n-old\n+new\n",
        );
        assert_eq!(a, b);
        assert_eq!(a.len(), 2);
    }
}
