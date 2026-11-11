// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Read-only operations for the code browser: refs, ref resolution, trees,
//! blobs, history, and diffs. User input only reaches Git as validated
//! full ref names or object IDs after `--end-of-options`, and as paths
//! after `--`.

use std::collections::HashSet;
use std::path::Path;

use crate::args::{RefName, RepoPath};
use crate::error::GitError;
use crate::object::{Blob, ObjectId, ObjectReader, TreeEntry};
use crate::runner::Runner;

/// The ID of the empty tree (base for a root commit's diff).
pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Branch,
    Tag,
    Commit,
}

/// A branch or tag with its commit.
#[derive(Debug, Clone)]
pub struct RefInfo {
    /// Short name (without `refs/heads/` or `refs/tags/`); may be non-UTF-8.
    pub name: Vec<u8>,
    pub commit: ObjectId,
    /// Unix time of the commit (branches) or tag.
    pub time: i64,
    pub subject: String,
}

/// Lists branches or tags, newest first, at most `limit`.
pub async fn list_refs(
    runner: &Runner,
    git_dir: &Path,
    kind: RefKind,
    limit: usize,
) -> Result<Vec<RefInfo>, GitError> {
    let prefix = match kind {
        RefKind::Branch => "refs/heads/",
        RefKind::Tag => "refs/tags/",
        RefKind::Commit => return Ok(Vec::new()),
    };
    let out = runner
        .repo(git_dir)
        .args([
            "for-each-ref",
            "--sort=-creatordate",
            &format!("--count={limit}"),
            "--format=%(objectname)%00%(*objectname)%00%(refname)%00%(creatordate:unix)%00%(contents:subject)",
            // Fork merge request branches aren't the repository's own.
            "--exclude=refs/heads/mr/",
            prefix,
        ])
        .run()
        .await?;
    let mut refs = Vec::new();
    for line in out.stdout.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        let f: Vec<&[u8]> = line.splitn(5, |&b| b == 0).collect();
        if f.len() < 5 {
            continue;
        }
        let target = if f[1].is_empty() { f[0] } else { f[1] };
        let Ok(commit) = ObjectId::parse(target) else {
            continue;
        };
        refs.push(RefInfo {
            name: f[2]
                .strip_prefix(prefix.as_bytes())
                .unwrap_or(f[2])
                .to_vec(),
            commit,
            time: std::str::from_utf8(f[3])
                .ok()
                .and_then(|t| t.parse().ok())
                .unwrap_or(0),
            subject: String::from_utf8_lossy(f[4]).into_owned(),
        });
    }
    Ok(refs)
}

/// The commit a full ref (or object ID) points to, or `None`.
pub async fn resolve_commit(
    runner: &Runner,
    git_dir: &Path,
    rev: &[u8],
) -> Result<Option<ObjectId>, GitError> {
    let mut arg = rev.to_vec();
    arg.extend_from_slice(b"^{commit}");
    let out = runner
        .repo(git_dir)
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
        .arg_bytes(&arg)
        .output()
        .await?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(ObjectId::parse(out.stdout.trim_ascii()).ok())
}

/// A `ref/path` from a URL, resolved.
#[derive(Debug, Clone)]
pub struct RefPath {
    pub kind: RefKind,
    /// The short ref name (or the object ID).
    pub name: Vec<u8>,
    pub commit: ObjectId,
    pub path: RepoPath,
}

/// Resolves `<ref>/<path>` by the longest existing branch, then tag, then a
/// full commit ID. Short names never reach Git: candidates are matched
/// against the listed refs first and then resolved as full ref names.
pub async fn resolve_ref_path(
    runner: &Runner,
    git_dir: &Path,
    ref_and_path: &[u8],
) -> Result<Option<RefPath>, GitError> {
    let segments: Vec<&[u8]> = ref_and_path.split(|&b| b == b'/').collect();
    if segments.is_empty() || segments[0].is_empty() {
        return Ok(None);
    }
    let out = runner
        .repo(git_dir)
        .args([
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads/",
            "refs/tags/",
        ])
        .run()
        .await?;
    let names: HashSet<&[u8]> = out
        .stdout
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .collect();
    for (prefix, kind) in [
        (&b"refs/heads/"[..], RefKind::Branch),
        (&b"refs/tags/"[..], RefKind::Tag),
    ] {
        for i in (1..=segments.len()).rev() {
            let short = segments[..i].join(&b'/');
            let mut full = prefix.to_vec();
            full.extend_from_slice(&short);
            if !names.contains(full.as_slice()) || RefName::parse(full.clone()).is_err() {
                continue;
            }
            let Some(commit) = resolve_commit(runner, git_dir, &full).await? else {
                return Ok(None);
            };
            let path =
                RepoPath::parse(segments[i..].join(&b'/')).map_err(GitError::InvalidArgument)?;
            return Ok(Some(RefPath {
                kind,
                name: short,
                commit,
                path,
            }));
        }
    }
    if let Ok(oid) = ObjectId::parse(segments[0])
        && let Some(commit) = resolve_commit(runner, git_dir, oid.as_str().as_bytes()).await?
    {
        let path = RepoPath::parse(segments[1..].join(&b'/')).map_err(GitError::InvalidArgument)?;
        return Ok(Some(RefPath {
            kind: RefKind::Commit,
            name: oid.as_str().as_bytes().to_vec(),
            commit,
            path,
        }));
    }
    Ok(None)
}

/// What a path in a commit's tree is.
#[derive(Debug, Clone)]
pub enum PathObject {
    Tree(Vec<TreeEntry>),
    Blob { entry: TreeEntry },
}

/// Walks `path` from the commit's root tree.
pub async fn lookup_path(
    reader: &mut ObjectReader,
    commit: &ObjectId,
    path: &RepoPath,
) -> Result<Option<PathObject>, GitError> {
    let raw = reader.read_raw_commit(commit).await?;
    let tree = raw
        .tree()
        .ok_or(GitError::Protocol("commit without tree"))?;
    let mut tree_oid = ObjectId::parse(tree).map_err(GitError::Protocol)?;
    if path.is_root() {
        return Ok(Some(PathObject::Tree(sorted(
            reader.read_tree(&tree_oid).await?,
        ))));
    }
    let parts: Vec<&[u8]> = path
        .as_bytes()
        .split(|&b| b == b'/')
        .filter(|p| !p.is_empty())
        .collect();
    for (i, part) in parts.iter().enumerate() {
        let entries = reader.read_tree(&tree_oid).await?;
        let Some(e) = entries.into_iter().find(|e| e.name.as_slice() == *part) else {
            return Ok(None);
        };
        let last = i + 1 == parts.len();
        match e.kind() {
            crate::object::EntryKind::Tree => {
                tree_oid = e.oid.clone();
                if last {
                    return Ok(Some(PathObject::Tree(sorted(
                        reader.read_tree(&tree_oid).await?,
                    ))));
                }
            }
            // Symlinks and submodules are never followed.
            _ if last => return Ok(Some(PathObject::Blob { entry: e })),
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// Directories first, then files, by name.
fn sorted(mut entries: Vec<TreeEntry>) -> Vec<TreeEntry> {
    entries.sort_by(|a, b| {
        let da = a.kind() == crate::object::EntryKind::Tree;
        let db = b.kind() == crate::object::EntryKind::Tree;
        db.cmp(&da).then_with(|| a.name.cmp(&b.name))
    });
    entries
}

/// Reads a blob if it's at most `limit` bytes.
pub async fn read_blob(
    reader: &mut ObjectReader,
    oid: &ObjectId,
    limit: usize,
) -> Result<Blob, GitError> {
    reader.read_blob(oid, limit).await
}

/// One commit in a history listing.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub oid: ObjectId,
    pub parents: Vec<ObjectId>,
    pub author_name: String,
    pub author_email: String,
    pub author_time: i64,
    pub subject: String,
}

/// History from `commit` (optionally limited to `path`), `skip`/`max` for paging.
pub async fn log(
    runner: &Runner,
    git_dir: &Path,
    commit: &ObjectId,
    path: &RepoPath,
    skip: usize,
    max: usize,
) -> Result<Vec<LogEntry>, GitError> {
    let mut inv = runner.repo(git_dir).args([
        "log",
        "--no-color",
        "-z",
        "--format=%H%x00%P%x00%an%x00%ae%x00%at%x00%s",
        &format!("--skip={skip}"),
        &format!("--max-count={max}"),
        "--end-of-options",
        commit.as_str(),
        "--",
    ]);
    if !path.is_root() {
        inv = inv.arg_bytes(path.as_bytes());
    }
    let out = inv.run().await?;
    Ok(parse_log(&out.stdout))
}

fn parse_log(stdout: &[u8]) -> Vec<LogEntry> {
    let fields: Vec<&[u8]> = stdout.split(|&b| b == 0).collect();
    let mut entries = Vec::new();
    for chunk in fields.chunks(6) {
        if chunk.len() < 6 {
            break;
        }
        let Ok(oid) = ObjectId::parse(chunk[0].trim_ascii()) else {
            continue;
        };
        entries.push(LogEntry {
            oid,
            parents: chunk[1]
                .split(|&b| b == b' ')
                .filter_map(|p| ObjectId::parse(p).ok())
                .collect(),
            author_name: String::from_utf8_lossy(chunk[2]).into_owned(),
            author_email: String::from_utf8_lossy(chunk[3]).into_owned(),
            author_time: std::str::from_utf8(chunk[4])
                .ok()
                .and_then(|t| t.parse().ok())
                .unwrap_or(0),
            subject: String::from_utf8_lossy(chunk[5]).into_owned(),
        });
    }
    entries
}

/// The commits reachable from `head` but not from `base` (all of them
/// without a base), oldest first, at most `max` (from the newest).
pub async fn log_range(
    runner: &Runner,
    git_dir: &Path,
    base: Option<&ObjectId>,
    head: &ObjectId,
    max: usize,
) -> Result<Vec<LogEntry>, GitError> {
    let range = match base {
        Some(b) => format!("{}..{}", b.as_str(), head.as_str()),
        None => head.as_str().to_owned(),
    };
    let out = runner
        .repo(git_dir)
        .args([
            "log",
            "--no-color",
            "-z",
            "--topo-order",
            "--format=%H%x00%P%x00%an%x00%ae%x00%at%x00%s",
            &format!("--max-count={max}"),
            "--end-of-options",
            &range,
            "--",
        ])
        .run()
        .await?;
    let mut entries = parse_log(&out.stdout);
    entries.reverse();
    Ok(entries)
}

/// The full messages of the commits reachable from `head` but not from
/// `base` (all of them without a base), oldest first, at most `max` (from
/// the newest).
pub async fn messages_range(
    runner: &Runner,
    git_dir: &Path,
    base: Option<&ObjectId>,
    head: &ObjectId,
    max: usize,
) -> Result<Vec<(ObjectId, String)>, GitError> {
    let range = match base {
        Some(b) => format!("{}..{}", b.as_str(), head.as_str()),
        None => head.as_str().to_owned(),
    };
    let out = runner
        .repo(git_dir)
        .args([
            "log",
            "--no-color",
            "-z",
            "--topo-order",
            "--format=%H%x00%B",
            &format!("--max-count={max}"),
            "--end-of-options",
            &range,
            "--",
        ])
        .run()
        .await?;
    let fields: Vec<&[u8]> = out.stdout.split(|&b| b == 0).collect();
    let mut entries: Vec<(ObjectId, String)> = fields
        .chunks(2)
        .filter(|c| c.len() == 2)
        .filter_map(|c| {
            let oid = ObjectId::parse(c[0].trim_ascii()).ok()?;
            Some((oid, String::from_utf8_lossy(c[1]).into_owned()))
        })
        .collect();
    entries.reverse();
    Ok(entries)
}

/// The best common ancestor of two commits, if they have one.
pub async fn merge_base(
    runner: &Runner,
    git_dir: &Path,
    a: &ObjectId,
    b: &ObjectId,
) -> Result<Option<ObjectId>, GitError> {
    let out = runner
        .repo(git_dir)
        .args(["merge-base", "--end-of-options", a.as_str(), b.as_str()])
        .output()
        .await?;
    match out.status.code() {
        Some(0) => ObjectId::parse(out.stdout.trim_ascii())
            .map(Some)
            .map_err(GitError::Protocol),
        Some(1) => Ok(None),
        _ => Err(GitError::Failed {
            status: out.status,
            stderr: out.stderr,
        }),
    }
}

/// How a diff is computed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffOptions {
    /// `--ignore-all-space`.
    pub ignore_whitespace: bool,
}

/// The unified diff from `base` (or the empty tree) to `commit`, and
/// whether it was cut at `max_bytes` (then empty).
pub async fn commit_diff(
    runner: &Runner,
    git_dir: &Path,
    base: Option<&ObjectId>,
    commit: &ObjectId,
    max_bytes: usize,
) -> Result<(Vec<u8>, bool), GitError> {
    diff(
        runner,
        git_dir,
        base,
        commit,
        DiffOptions::default(),
        max_bytes,
    )
    .await
}

/// Like [`commit_diff`], with options.
pub async fn diff(
    runner: &Runner,
    git_dir: &Path,
    base: Option<&ObjectId>,
    commit: &ObjectId,
    opts: DiffOptions,
    max_bytes: usize,
) -> Result<(Vec<u8>, bool), GitError> {
    let base = base.map_or(EMPTY_TREE, ObjectId::as_str);
    let mut cmd = runner
        .repo(git_dir)
        .config("core.quotePath", "false")
        .args([
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "-M",
            "--full-index",
            "--patch",
        ]);
    if opts.ignore_whitespace {
        cmd = cmd.arg("--ignore-all-space");
    }
    let result = cmd
        .args(["--end-of-options", base, commit.as_str()])
        .max_output(max_bytes)
        .run()
        .await;
    match result {
        Ok(out) => Ok((out.stdout, false)),
        Err(GitError::Limit(crate::LimitKind::Output)) => Ok((Vec::new(), true)),
        Err(e) => Err(e),
    }
}

/// Which of `paths` are marked generated in `source`'s `.gitattributes`
/// (`linguist-generated` or `slath-generated` set).
pub async fn generated_paths(
    runner: &Runner,
    git_dir: &Path,
    source: &ObjectId,
    paths: &[Vec<u8>],
) -> Result<std::collections::HashSet<Vec<u8>>, GitError> {
    let mut out = std::collections::HashSet::new();
    if paths.is_empty() {
        return Ok(out);
    }
    let mut input = Vec::new();
    for p in paths {
        input.extend_from_slice(p);
        input.push(0);
    }
    let res = runner
        .repo(git_dir)
        .arg(format!("--attr-source={}", source.as_str()))
        .args([
            "check-attr",
            "-z",
            "--stdin",
            "linguist-generated",
            "slath-generated",
        ])
        .stdin(input)
        .run()
        .await?;
    // NUL-separated triples: path, attribute, value.
    let mut it = res.stdout.split(|b| *b == 0);
    while let (Some(path), Some(_), Some(value)) = (it.next(), it.next(), it.next()) {
        if matches!(value, b"set" | b"true") {
            out.insert(path.to_vec());
        }
    }
    Ok(out)
}

/// `git range-diff` between two commit ranges (`base..head` each), as
/// plain text.
pub async fn range_diff(
    runner: &Runner,
    git_dir: &Path,
    old: (&ObjectId, &ObjectId),
    new: (&ObjectId, &ObjectId),
    creation_factor: u32,
    max_bytes: usize,
) -> Result<(Vec<u8>, bool), GitError> {
    let result = runner
        .repo(git_dir)
        .config("core.quotePath", "false")
        .args([
            "range-diff",
            "--no-color",
            "--no-notes",
            &format!("--creation-factor={creation_factor}"),
            "--end-of-options",
            &format!("{}..{}", old.0.as_str(), old.1.as_str()),
            &format!("{}..{}", new.0.as_str(), new.1.as_str()),
        ])
        .max_output(max_bytes)
        .run()
        .await;
    match result {
        Ok(out) => Ok((out.stdout, false)),
        Err(GitError::Limit(crate::LimitKind::Output)) => Ok((Vec::new(), true)),
        Err(e) => Err(e),
    }
}

/// One line of `git blame` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameLine {
    /// The commit that last changed the line.
    pub commit: ObjectId,
    /// Line number in that commit's version of the file.
    pub orig_line: usize,
    pub text: Vec<u8>,
}

/// What blame shows about a commit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BlameCommit {
    pub author_name: String,
    pub author_time: i64,
    pub summary: String,
}

/// Blame of a file: one entry per line, and the commits they come from.
#[derive(Debug, Clone, Default)]
pub struct Blame {
    pub lines: Vec<BlameLine>,
    pub commits: std::collections::HashMap<ObjectId, BlameCommit>,
}

/// Parses `git blame --porcelain` output.
pub fn parse_blame(out: &[u8]) -> Result<Blame, GitError> {
    let bad = || GitError::Protocol("unexpected blame output");
    let mut blame = Blame::default();
    let mut current: Option<(ObjectId, usize)> = None;
    for line in out.split(|&b| b == b'\n') {
        if let Some(text) = line.strip_prefix(b"\t") {
            let (commit, orig) = current.take().ok_or_else(bad)?;
            blame.lines.push(BlameLine {
                commit,
                orig_line: orig,
                text: text.to_vec(),
            });
            continue;
        }
        if line.is_empty() {
            continue;
        }
        if current.is_none() {
            // Header: `<oid> <orig> <final> [<count>]`.
            let mut parts = line.split(|&b| b == b' ');
            let oid = ObjectId::parse(parts.next().ok_or_else(bad)?).map_err(|_| bad())?;
            let orig = parts
                .next()
                .and_then(|p| std::str::from_utf8(p).ok())
                .and_then(|p| p.parse().ok())
                .ok_or_else(bad)?;
            blame.commits.entry(oid.clone()).or_default();
            current = Some((oid, orig));
            continue;
        }
        // Commit information, given once per commit.
        let Some((oid, _)) = &current else { continue };
        let (key, value) = match line.iter().position(|&b| b == b' ') {
            Some(i) => (&line[..i], &line[i + 1..]),
            None => (line, &b""[..]),
        };
        let c = blame.commits.entry(oid.clone()).or_default();
        let value = String::from_utf8_lossy(value).into_owned();
        match key {
            b"author" => c.author_name = value,
            b"author-time" => c.author_time = value.parse().unwrap_or(0),
            b"summary" => c.summary = value,
            _ => {}
        }
    }
    Ok(blame)
}

/// `git blame --porcelain` of `path` at `commit`, within the runner's
/// timeout and output limits.
pub async fn blame(
    runner: &Runner,
    git_dir: &Path,
    commit: &ObjectId,
    path: &RepoPath,
) -> Result<Blame, GitError> {
    let out = runner
        .repo(git_dir)
        // `--end-of-options` confuses blame's argument parsing; the commit
        // is a validated object ID, so it can't be taken for an option.
        .args([
            "blame",
            "--porcelain",
            "--no-progress",
            commit.as_str(),
            "--",
        ])
        .arg_bytes(path.as_bytes())
        .run()
        .await?;
    parse_blame(&out.stdout)
}

#[cfg(test)]
mod blame_tests {
    use super::*;

    #[test]
    fn porcelain_is_parsed() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let out = format!(
            "{a} 1 1 2\nauthor Ann\nauthor-mail <ann@example.org>\nauthor-time 1700000000\nsummary First\nfilename f\n\tone\n{a} 2 2\n\ttwo\n{b} 1 3 1\nauthor Bob\nauthor-time 1700000100\nsummary Second line\nprevious {a} f\nfilename f\n\t\tthree\n"
        );
        let blame = parse_blame(out.as_bytes()).unwrap();
        assert_eq!(blame.lines.len(), 3);
        assert_eq!(blame.lines[1].text, b"two");
        assert_eq!(blame.lines[2].text, b"\tthree");
        assert_eq!(blame.lines[2].orig_line, 1);
        let ca = &blame.commits[&ObjectId::parse(a.as_bytes()).unwrap()];
        assert_eq!(ca.author_name, "Ann");
        assert_eq!(ca.summary, "First");
        assert_eq!(
            blame.commits[&blame.lines[2].commit].author_time,
            1_700_000_100
        );
        assert!(parse_blame(b"\torphan line\n").is_err());
    }
}
