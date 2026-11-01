// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath backup create` and `slath backup restore`.
//!
//! A backup is a directory:
//!
//! ```text
//! manifest.json            versions, counts, tables, and a SHA-256 per file
//! config.toml              the configuration, secrets removed by default
//! db/<table>.copy          one consistent database snapshot
//! repos/<id>.git/…         refs, packed-refs, and objects of live repositories
//! trash/<id>.git/…         the same for deleted repositories awaiting purge
//! ```
//!
//! Backups run while the server is serving. Refs are copied before objects,
//! so every copied ref points to copied objects; differences between the
//! database snapshot and the copied refs are repaired by the reconciler
//! after a restore. Restores trust nothing in the directory: every path is
//! checked, every file must be listed with a matching checksum, repository
//! paths are derived from validated IDs, and repository config and hooks are
//! never taken from a backup.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use slath_core::ids::RepoId;

use crate::config;
use crate::output::{OutputFormat, fail, print_json, runtime};

const FORMAT: u32 = 1;
const MANIFEST: &str = "manifest.json";
const MAX_MANIFEST_BYTES: u64 = 256 * 1024 * 1024;
/// Attempts to copy a repository that changes underneath the copy.
const COPY_ATTEMPTS: usize = 5;

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    format: u32,
    slath_version: String,
    schema_version: i32,
    created_at: String,
    git_version: String,
    pg_version: String,
    includes_secrets: bool,
    counts: BTreeMap<String, u64>,
    tables: Vec<slath_db::backup::TableInfo>,
    sequences: Vec<slath_db::backup::SequenceInfo>,
    repositories: Vec<RepoEntry>,
    /// Relative path → lowercase hex SHA-256.
    files: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RepoEntry {
    id: String,
    /// `live` or `trash`.
    location: String,
    /// The directory was gone when the backup ran (deleted meanwhile).
    #[serde(default)]
    missing: bool,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Writes files into the backup directory and records their checksums.
struct Writer {
    root: PathBuf,
    files: BTreeMap<String, String>,
}

impl Writer {
    fn create_file(&self, rel: &str) -> Result<std::fs::File, String> {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("cannot create {}: {e}", path.display()))
    }

    fn write(&mut self, rel: &str, data: &[u8]) -> Result<(), String> {
        let mut f = self.create_file(rel)?;
        f.write_all(data)
            .and_then(|()| f.sync_all())
            .map_err(|e| format!("cannot write {rel}: {e}"))?;
        self.files
            .insert(rel.to_owned(), hex(&Sha256::digest(data)));
        Ok(())
    }

    /// Copies `src` to `rel`. `Ok(false)` if `src` vanished.
    fn copy(&mut self, src: &Path, rel: &str) -> Result<bool, String> {
        let mut input = match std::fs::File::open(src) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(format!("cannot read {}: {e}", src.display())),
        };
        let mut out = self.create_file(rel)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = input
                .read(&mut buf)
                .map_err(|e| format!("cannot read {}: {e}", src.display()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])
                .map_err(|e| format!("cannot write {rel}: {e}"))?;
        }
        out.sync_all()
            .map_err(|e| format!("cannot write {rel}: {e}"))?;
        self.files.insert(rel.to_owned(), hex(&hasher.finalize()));
        Ok(true)
    }

    fn forget_prefix(&mut self, prefix: &str) {
        self.files.retain(|k, _| !k.starts_with(prefix));
    }
}

/// Whether a path inside a repository belongs in a backup (and may come
/// back from one).
fn repo_file_allowed(rel: &[&str]) -> bool {
    match rel {
        ["packed-refs"] => true,
        ["refs", rest @ ..] => !rest.is_empty(),
        ["objects", "info", "alternates" | "http-alternates"] => false,
        ["objects", rest @ ..] => {
            !rest.is_empty()
                && !rest.iter().any(|c| {
                    c.starts_with("tmp_") || c.starts_with("incoming-") || c.ends_with(".lock")
                })
        }
        _ => false,
    }
}

/// Regular files under `dir`, as paths relative to `dir`, sorted.
fn list_files(dir: &Path) -> Result<Vec<Vec<String>>, String> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), Vec::<String>::new())];
    while let Some((d, prefix)) = stack.pop() {
        let entries = match std::fs::read_dir(&d) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot list {}: {e}", d.display())),
        };
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Ok(ft) = entry.file_type() else { continue };
            let mut rel = prefix.clone();
            rel.push(name);
            if ft.is_dir() {
                stack.push((entry.path(), rel));
            } else if ft.is_file() {
                out.push(rel);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Copies one repository: refs first, then objects. `Ok(false)` if files
/// disappeared during the copy (a concurrent repack); the caller retries.
fn copy_repo(w: &mut Writer, src: &Path, prefix: &str) -> Result<bool, String> {
    let mut refs = Vec::new();
    if src.join("packed-refs").is_file() {
        refs.push(vec!["packed-refs".to_owned()]);
    }
    refs.extend(list_files(&src.join("refs"))?.into_iter().map(|mut r| {
        r.insert(0, "refs".to_owned());
        r
    }));
    for rel in refs {
        let parts: Vec<&str> = rel.iter().map(String::as_str).collect();
        if !repo_file_allowed(&parts) {
            continue;
        }
        let joined = rel.join("/");
        // A ref deleted meanwhile is simply not part of the backup.
        w.copy(&src.join(&joined), &format!("{prefix}/{joined}"))?;
    }
    for mut rel in list_files(&src.join("objects"))? {
        rel.insert(0, "objects".to_owned());
        let parts: Vec<&str> = rel.iter().map(String::as_str).collect();
        if !repo_file_allowed(&parts) {
            continue;
        }
        let joined = rel.join("/");
        if !w.copy(&src.join(&joined), &format!("{prefix}/{joined}"))? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The configuration with passwords, secrets, and tokens removed.
fn redact_config(text: &str) -> Result<String, String> {
    fn walk(v: &mut toml::Value) {
        if let toml::Value::Table(t) = v {
            t.retain(|k, _| {
                let k = k.to_ascii_lowercase();
                !(k.contains("password") || k.contains("secret") || k.contains("token"))
            });
            for (k, val) in t.iter_mut() {
                if k == "url"
                    && let toml::Value::String(s) = val
                {
                    *s = strip_url_password(s);
                }
                walk(val);
            }
        }
    }
    let mut v: toml::Value =
        toml::from_str(text).map_err(|e| format!("cannot parse the configuration: {e}"))?;
    walk(&mut v);
    toml::to_string(&v).map_err(|e| e.to_string())
}

/// `scheme://user:pass@host/…` → `scheme://user@host/…`.
fn strip_url_password(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    match authority.rsplit_once('@') {
        Some((userinfo, host)) => {
            let user = userinfo.split_once(':').map_or(userinfo, |(u, _)| u);
            format!("{scheme}://{user}@{host}{tail}")
        }
        None => url.to_owned(),
    }
}

/// Repository IDs and states from the dumped `repositories` table.
fn dumped_repos(
    info: &slath_db::backup::DumpInfo,
    data: &[u8],
) -> Result<Vec<(RepoId, bool)>, String> {
    let Some(t) = info.tables.iter().find(|t| t.name == "repositories") else {
        return Ok(Vec::new());
    };
    let col = |name: &str| {
        t.columns
            .iter()
            .position(|c| c == name)
            .ok_or_else(|| format!("repositories has no {name} column"))
    };
    let (id_col, state_col) = (col("id")?, col("state")?);
    let mut out = Vec::new();
    for line in data.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let fields: Vec<&[u8]> = line.split(|b| *b == b'\t').collect();
        let id = fields
            .get(id_col)
            .and_then(|f| std::str::from_utf8(f).ok())
            .and_then(|s| s.parse::<RepoId>().ok())
            .ok_or("unreadable repository ID in the dump")?;
        let deleting = fields.get(state_col).is_some_and(|s| *s == b"deleting");
        out.push((id, deleting));
    }
    Ok(out)
}

pub fn create(
    config_path: &Path,
    output: &Path,
    include_secrets: bool,
    format: OutputFormat,
) -> ExitCode {
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return fail(format, &e),
    };
    match rt.block_on(create_inner(config_path, output, include_secrets)) {
        Ok(m) => {
            match format {
                OutputFormat::Text => {
                    println!("backup written to {}", output.display());
                    for (k, v) in &m.counts {
                        println!("  {k}: {v}");
                    }
                    println!(
                        "The master key is not part of the backup; keep a copy of it separately."
                    );
                }
                OutputFormat::Json => print_json(&json!({
                    "ok": true,
                    "output": output.display().to_string(),
                    "counts": m.counts,
                    "files": m.files.len(),
                })),
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(format, &e),
    }
}

async fn create_inner(
    config_path: &Path,
    output: &Path,
    include_secrets: bool,
) -> Result<Manifest, String> {
    let cfg = config::load(config_path)?;
    match std::fs::read_dir(output) {
        Ok(mut d) => {
            if d.next().is_some() {
                return Err(format!("{} is not empty", output.display()));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(output)
                .map_err(|e| format!("cannot create {}: {e}", output.display()))?;
        }
        Err(e) => return Err(format!("cannot open {}: {e}", output.display())),
    }
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(output, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot restrict {}: {e}", output.display()))?;
    }
    let mut pc = slath_db::PoolConfig::new(config::database_url(&cfg)?);
    pc.max_connections = 2;
    let pool = slath_db::connect(&pc).await.map_err(|e| e.to_string())?;
    slath_db::check_schema(&pool)
        .await
        .map_err(|e| e.to_string())?;
    let runner = runner(&cfg)?;
    let git_version = runner
        .check_version()
        .await
        .map_err(|e| e.to_string())?
        .to_string();

    let mut w = Writer {
        root: output.to_path_buf(),
        files: BTreeMap::new(),
    };
    // The database snapshot.
    let mut open: Option<(String, std::fs::File, Sha256)> = None;
    let mut repos_data = Vec::new();
    let mut finished: Vec<(String, String)> = Vec::new();
    let info = slath_db::backup::dump(&pool, |table, chunk| {
        if !table
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(std::io::Error::other(format!(
                "unexpected table name {table:?}"
            )));
        }
        if open.as_ref().is_none_or(|(t, _, _)| t != table) {
            if let Some((t, f, h)) = open.take() {
                f.sync_all()?;
                finished.push((t, hex(&h.finalize())));
            }
            let f = w
                .create_file(&format!("db/{table}.copy"))
                .map_err(std::io::Error::other)?;
            open = Some((table.to_owned(), f, Sha256::new()));
        }
        if let Some((_, f, h)) = open.as_mut() {
            f.write_all(chunk)?;
            h.update(chunk);
        }
        if table == "repositories" {
            repos_data.extend_from_slice(chunk);
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("database dump failed: {e}"))?;
    if let Some((t, f, h)) = open.take() {
        f.sync_all().map_err(|e| e.to_string())?;
        finished.push((t, hex(&h.finalize())));
    }
    for (t, sum) in finished {
        w.files.insert(format!("db/{t}.copy"), sum);
    }

    // Repositories, as of the snapshot.
    let mut repositories = Vec::new();
    for (id, deleting) in dumped_repos(&info, &repos_data)? {
        let live = slath_git::storage::repo_path(&cfg.storage.data_dir, id);
        let trash = cfg.storage.data_dir.join("trash").join(format!("{id}.git"));
        let src = if deleting && trash.is_dir() {
            trash
        } else {
            live
        };
        let location = if deleting { "trash" } else { "live" };
        let prefix = format!("{}/{id}.git", if deleting { "trash" } else { "repos" });
        if !src.is_dir() {
            repositories.push(RepoEntry {
                id: id.to_string(),
                location: location.into(),
                missing: true,
            });
            continue;
        }
        let mut done = false;
        for _ in 0..COPY_ATTEMPTS {
            w.forget_prefix(&format!("{prefix}/"));
            let _ = std::fs::remove_dir_all(output.join(&prefix));
            if copy_repo(&mut w, &src, &prefix)? {
                done = true;
                break;
            }
        }
        if !done {
            return Err(format!(
                "repository {id} kept changing during the backup; try again"
            ));
        }
        repositories.push(RepoEntry {
            id: id.to_string(),
            location: location.into(),
            missing: false,
        });
    }

    let text = std::fs::read_to_string(config_path)
        .map_err(|e| format!("cannot read {}: {e}", config_path.display()))?;
    let config_text = if include_secrets {
        text
    } else {
        redact_config(&text)?
    };
    w.write("config.toml", config_text.as_bytes())?;

    let rows = |t: &str| {
        info.tables
            .iter()
            .find(|x| x.name == t)
            .map_or(0, |x| x.rows)
    };
    let counts = BTreeMap::from([
        ("users".to_owned(), rows("users")),
        ("organizations".to_owned(), rows("organizations")),
        ("repositories".to_owned(), rows("repositories")),
        ("refs".to_owned(), rows("git_refs")),
    ]);
    let manifest = Manifest {
        format: FORMAT,
        slath_version: env!("CARGO_PKG_VERSION").to_owned(),
        schema_version: info.schema_version,
        created_at: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
        git_version,
        pg_version: info.server_version.clone(),
        includes_secrets: include_secrets,
        counts,
        tables: info.tables,
        sequences: info.sequences,
        repositories,
        files: w.files,
    };
    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let tmp = output.join(".manifest.json.tmp");
    std::fs::write(&tmp, &json).map_err(|e| format!("cannot write the manifest: {e}"))?;
    std::fs::rename(&tmp, output.join(MANIFEST))
        .map_err(|e| format!("cannot write the manifest: {e}"))?;
    Ok(manifest)
}

fn runner(cfg: &config::Loaded) -> Result<slath_git::Runner, String> {
    let home = cfg.storage.data_dir.join("git-home");
    std::fs::create_dir_all(&home).map_err(|e| format!("cannot create {}: {e}", home.display()))?;
    slath_git::Runner::new(slath_git::RunnerConfig::new(cfg.git.binary.clone(), home))
        .map_err(|e| e.to_string())
}

/// Splits a manifest path into plain components, refusing anything else.
fn safe_components(rel: &str) -> Result<Vec<&str>, String> {
    let bad = || format!("unsafe path in the backup: {rel:?}");
    if rel.is_empty() || rel.contains('\0') || rel.contains('\\') {
        return Err(bad());
    }
    let mut out = Vec::new();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(s) => out.push(s.to_str().ok_or_else(bad)?),
            _ => return Err(bad()),
        }
    }
    if out.join("/") != rel {
        return Err(bad());
    }
    Ok(out)
}

/// What a listed file is, after validation.
enum Entry {
    Config,
    Table(String),
    Repo {
        id: RepoId,
        trash: bool,
        inner: String,
    },
}

fn classify(rel: &str) -> Result<Entry, String> {
    let parts = safe_components(rel)?;
    let bad = || format!("unexpected file in the backup: {rel:?}");
    match parts.as_slice() {
        ["config.toml"] => Ok(Entry::Config),
        ["db", file] => {
            let name = file.strip_suffix(".copy").ok_or_else(bad)?;
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                return Err(bad());
            }
            Ok(Entry::Table(name.to_owned()))
        }
        [area @ ("repos" | "trash"), dir, rest @ ..] => {
            let id: RepoId = dir
                .strip_suffix(".git")
                .and_then(|s| s.parse().ok())
                .ok_or_else(bad)?;
            if id.to_string() + ".git" != *dir || !repo_file_allowed(rest) {
                return Err(bad());
            }
            Ok(Entry::Repo {
                id,
                trash: *area == "trash",
                inner: rest.join("/"),
            })
        }
        _ => Err(bad()),
    }
}

/// Every file in the backup directory, relative, after checking that the
/// tree holds only directories and regular, singly linked files.
fn walk_backup(root: &Path) -> Result<Vec<String>, String> {
    use std::os::unix::fs::MetadataExt;
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?
        {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|n| format!("non-UTF-8 name in the backup: {n:?}"))?;
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let meta = std::fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
            if meta.file_type().is_symlink() {
                return Err(format!("the backup contains a symbolic link: {rel}"));
            } else if meta.is_dir() {
                stack.push((entry.path(), rel));
            } else if meta.is_file() {
                if meta.nlink() > 1 {
                    return Err(format!("the backup contains a hard link: {rel}"));
                }
                out.push(rel);
            } else {
                return Err(format!("the backup contains a special file: {rel}"));
            }
        }
    }
    out.sort();
    Ok(out)
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f =
        std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if n == 0 {
            return Ok(hex(&h.finalize()));
        }
        h.update(&buf[..n]);
    }
}

/// Reads and fully verifies a backup before anything is written.
fn verify(input: &Path) -> Result<Manifest, String> {
    let path = input.join(MANIFEST);
    let meta = std::fs::symlink_metadata(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if !meta.is_file() || meta.len() > MAX_MANIFEST_BYTES {
        return Err("the manifest is not a regular file of acceptable size".into());
    }
    let manifest: Manifest =
        serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("the manifest is invalid: {e}"))?;
    if manifest.format != FORMAT {
        return Err(format!("unsupported backup format {}", manifest.format));
    }
    let present = walk_backup(input)?;
    for rel in &present {
        if rel != MANIFEST && !manifest.files.contains_key(rel) {
            return Err(format!(
                "the backup contains a file the manifest doesn't list: {rel}"
            ));
        }
    }
    for (rel, sum) in &manifest.files {
        if let Entry::Table(name) = classify(rel)?
            && !manifest.tables.iter().any(|t| t.name == name)
        {
            return Err(format!(
                "the backup contains data for an unlisted table: {rel}"
            ));
        }
        if !present.contains(rel) {
            return Err(format!("the backup is missing {rel}"));
        }
        if sha256_file(&input.join(rel))? != *sum {
            return Err(format!("checksum mismatch: {rel}"));
        }
    }
    for t in &manifest.tables {
        if !manifest.files.contains_key(&format!("db/{}.copy", t.name)) {
            return Err(format!(
                "the backup is missing the data of table {}",
                t.name
            ));
        }
    }
    for r in &manifest.repositories {
        let id: RepoId =
            r.id.parse()
                .map_err(|_| format!("invalid repository ID {:?}", r.id))?;
        if id.to_string() != r.id || !matches!(r.location.as_str(), "live" | "trash") {
            return Err(format!("invalid repository entry {:?}", r.id));
        }
    }
    Ok(manifest)
}

pub fn restore(config_path: &Path, input: &Path, format: OutputFormat) -> ExitCode {
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return fail(format, &e),
    };
    match rt.block_on(restore_inner(config_path, input)) {
        Ok((manifest, counts)) => {
            let ok = manifest
                .counts
                .iter()
                .all(|(k, v)| k == "refs" || counts.get(k) == Some(v));
            match format {
                OutputFormat::Text => {
                    println!("restored from {}", input.display());
                    for (k, v) in &counts {
                        let want = manifest.counts.get(k).copied().unwrap_or_default();
                        println!("  {k}: {v} (backup: {want})");
                    }
                    if !ok {
                        println!("counts differ from the backup");
                    }
                }
                OutputFormat::Json => print_json(&json!({
                    "ok": ok,
                    "counts": counts,
                    "backup_counts": manifest.counts,
                })),
            }
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => fail(format, &e),
    }
}

fn dir_is_empty(path: &Path) -> Result<bool, String> {
    match std::fs::read_dir(path) {
        Ok(mut d) => Ok(d.next().is_none()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(format!("cannot open {}: {e}", path.display())),
    }
}

async fn restore_inner(
    config_path: &Path,
    input: &Path,
) -> Result<(Manifest, BTreeMap<String, u64>), String> {
    let cfg = config::load(config_path)?;
    let manifest = verify(input)?;
    let data = &cfg.storage.data_dir;
    for sub in ["repos", "trash"] {
        if !dir_is_empty(&data.join(sub))? {
            return Err(format!(
                "{} is not empty; restore needs an empty data directory",
                data.join(sub).display()
            ));
        }
    }
    let mut pc = slath_db::PoolConfig::new(config::database_url(&cfg)?);
    pc.max_connections = 2;
    let pool = slath_db::connect(&pc).await.map_err(|e| e.to_string())?;
    if !slath_db::backup::is_empty(&pool)
        .await
        .map_err(|e| e.to_string())?
    {
        return Err("the database is not empty; restore needs an empty database".into());
    }
    let runner = runner(&cfg)?;
    runner.check_version().await.map_err(|e| e.to_string())?;

    let info = slath_db::backup::DumpInfo {
        schema_version: manifest.schema_version,
        server_version: manifest.pg_version.clone(),
        tables: manifest.tables.clone(),
        sequences: manifest.sequences.clone(),
    };
    slath_db::backup::restore(&pool, &info, |table| {
        let path = input.join("db").join(format!("{table}.copy"));
        async move { tokio::fs::File::open(path).await }
    })
    .await
    .map_err(|e| format!("restoring the database failed: {e}"))?;

    // Repositories: fresh ones with Slath's own config, then refs and objects.
    let branches: BTreeMap<String, String> = slath_db::repos::default_branches(&pool)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(id, b)| (id.to_string(), b))
        .collect();
    let mut by_repo: BTreeMap<(RepoId, bool), Vec<String>> = BTreeMap::new();
    for rel in manifest.files.keys() {
        if let Entry::Repo { id, trash, inner } = classify(rel)? {
            by_repo.entry((id, trash)).or_default().push(inner);
        }
    }
    for r in manifest.repositories.iter().filter(|r| !r.missing) {
        let id: RepoId =
            r.id.parse()
                .map_err(|_| "invalid repository ID".to_owned())?;
        let trash = r.location == "trash";
        let branch = branches
            .get(&r.id)
            .ok_or_else(|| format!("repository {id} is in the manifest but not in the database"))?;
        let path = slath_git::storage::create_bare(&runner, data, id, branch)
            .await
            .map_err(|e| format!("cannot create repository {id}: {e}"))?;
        let area = if trash { "trash" } else { "repos" };
        for inner in by_repo.get(&(id, trash)).into_iter().flatten() {
            let dest = path.join(inner);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::copy(
                input.join(area).join(format!("{id}.git")).join(inner),
                &dest,
            )
            .map_err(|e| format!("cannot restore {}: {e}", dest.display()))?;
        }
        slath_git::storage::check_connectivity(&runner, &path)
            .await
            .map_err(|e| format!("repository {id} is incomplete: {e}"))?;
        if trash {
            let target = data.join("trash").join(format!("{id}.git"));
            std::fs::create_dir_all(data.join("trash")).map_err(|e| e.to_string())?;
            std::fs::rename(&path, &target).map_err(|e| e.to_string())?;
        }
    }

    // Repair derived state against the restored refs.
    let key = slath_auth::MasterKey::generate().map_err(|e| e.to_string())?;
    let services = slath_service::Services::new(pool.clone(), Arc::new(cfg.cfg), &key, runner);
    let checks = services
        .fsck(None, true)
        .await
        .map_err(|e| crate::admin::describe(&e))?;
    let problems: Vec<String> = checks
        .iter()
        .filter(|c| !(c.is_clean() || c.repaired) && !c.missing_on_disk)
        .map(|c| c.name.clone())
        .collect();
    if !problems.is_empty() {
        return Err(format!(
            "fsck found problems after the restore: {}",
            problems.join(", ")
        ));
    }
    let counts = slath_db::repos::entity_counts(&pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok((
        manifest,
        counts.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_files_are_restricted() {
        assert!(repo_file_allowed(&["refs", "heads", "main"]));
        assert!(repo_file_allowed(&["packed-refs"]));
        assert!(repo_file_allowed(&["objects", "pack", "pack-1.pack"]));
        for bad in [
            &["config"][..],
            &["hooks", "pre-receive"],
            &["objects", "info", "alternates"],
            &["objects", "tmp_objdir-incoming-x", "ab"],
            &["HEAD"],
            &["refs"],
        ] {
            assert!(!repo_file_allowed(bad), "{bad:?}");
        }
    }

    #[test]
    fn manifest_paths_must_be_plain() {
        for bad in [
            "../etc/passwd",
            "/etc/passwd",
            "db/../../x",
            "repos/./x",
            "db//users.copy",
            "db/users.copy/",
            "db\\users.copy",
            "repos/not-a-uuid.git/refs/heads/main",
            "repos/0192f1a2-3b4c-7d5e-8f60-718293a4b5c6.git/config",
            "repos/0192F1A2-3B4C-7D5E-8F60-718293A4B5C6.git/refs/heads/x",
            "db/Users.copy",
            "evil.sh",
        ] {
            assert!(classify(bad).is_err(), "{bad}");
        }
        assert!(classify("db/users.copy").is_ok());
        assert!(classify("repos/0192f1a2-3b4c-7d5e-8f60-718293a4b5c6.git/refs/heads/main").is_ok());
    }

    #[test]
    fn config_secrets_are_removed() {
        let text = "[database]\nurl = \"postgres://slath:hunter2@db:5432/slath\"\npassword_file = \"/x\"\n[secrets]\nmaster_key_file = \"/k\"\n[server]\nlisten = \"0.0.0.0:3000\"\n";
        let out = redact_config(text).unwrap();
        assert!(!out.contains("hunter2"), "{out}");
        assert!(out.contains("postgres://slath@db:5432/slath"), "{out}");
        assert!(!out.contains("password_file"));
        assert!(
            !out.contains("[secrets]") || !out.contains("master_key_file"),
            "{out}"
        );
        assert!(out.contains("0.0.0.0:3000"));
    }
}
