// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath setup [--check]`: turns the binary into a running instance on a
//! systemd host. Every step looks at the current state first and changes
//! only what differs, so a rerun repairs drift and changes nothing else.
//! Existing configuration, keys, and database URLs are never rewritten.

mod system;

use std::io::{BufRead, IsTerminal, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::{Deserialize, Serialize};
use serde_json::json;
use slath_core::Secret;

use crate::output::{OutputFormat, print_json, runtime};

const SYSUSERS: &str = include_str!("../../../../deploy/systemd/sysusers-slath.conf");
const UNIT: &str = include_str!("../../../../deploy/systemd/slath.service");
const SSHD: &str = include_str!("../../../../deploy/sshd/slath.conf");

/// Where everything goes. Fixed on a real host; tests don't change it
/// (they run in a container that plays the host).
struct Paths {
    sysusers: PathBuf,
    etc: PathBuf,
    data: PathBuf,
    unit: PathBuf,
    sshd_dir: PathBuf,
}

impl Paths {
    fn host() -> Self {
        Self {
            sysusers: "/usr/lib/sysusers.d/slath.conf".into(),
            etc: "/etc/slath".into(),
            data: "/var/lib/slath".into(),
            unit: "/etc/systemd/system/slath.service".into(),
            sshd_dir: "/etc/ssh/sshd_config.d".into(),
        }
    }
    fn config(&self) -> PathBuf {
        self.etc.join("slath.toml")
    }
    fn master_key(&self) -> PathBuf {
        self.etc.join("master.key")
    }
    fn db_url(&self) -> PathBuf {
        self.etc.join("db-url")
    }
    fn sshd(&self) -> PathBuf {
        self.sshd_dir.join("slath.conf")
    }
}

pub struct Opts {
    pub check: bool,
    pub public_url: Option<String>,
    pub listen: String,
    pub ssh_port: u16,
    pub admin_username: Option<String>,
    pub admin_email: Option<String>,
    pub pg_admin_url: Option<String>,
    pub binary: Option<PathBuf>,
    pub output: OutputFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    /// Already as wanted.
    Ok,
    /// Changed (or, with `--check`, would be changed).
    Change,
    /// Needs the operator; the detail says how.
    Manual,
}

#[derive(Debug, Serialize)]
struct Entry {
    step: &'static str,
    kind: Kind,
    detail: String,
}

struct Run {
    opts: Opts,
    paths: Paths,
    binary: PathBuf,
    entries: Vec<Entry>,
    /// The users exist (with `--check`, they may not yet).
    users: Option<Ids>,
}

#[derive(Clone, Copy)]
struct Ids {
    slath_uid: u32,
    slath_gid: u32,
}

impl Run {
    fn ok(&mut self, step: &'static str, detail: impl Into<String>) {
        self.entries.push(Entry {
            step,
            kind: Kind::Ok,
            detail: detail.into(),
        });
    }
    fn change(&mut self, step: &'static str, detail: impl Into<String>) {
        self.entries.push(Entry {
            step,
            kind: Kind::Change,
            detail: detail.into(),
        });
    }
    fn manual(&mut self, step: &'static str, detail: impl Into<String>) {
        self.entries.push(Entry {
            step,
            kind: Kind::Manual,
            detail: detail.into(),
        });
    }
    fn apply(&self) -> bool {
        !self.opts.check
    }
}

/// `slath setup`.
pub fn run(opts: Opts) -> ExitCode {
    let format = opts.output;
    let check = opts.check;
    if !check && !nix::unistd::geteuid().is_root() {
        return crate::output::fail(format, "slath setup must run as root (or use --check)");
    }
    let binary = match opts
        .binary
        .clone()
        .map_or_else(std::env::current_exe, Ok)
        .and_then(std::fs::canonicalize)
    {
        Ok(b) => b,
        Err(e) => {
            return crate::output::fail(format, &format!("cannot locate the slath binary: {e}"));
        }
    };
    let mut r = Run {
        opts,
        paths: Paths::host(),
        binary,
        entries: Vec::new(),
        users: None,
    };
    let result = steps(&mut r);
    report(&r, result.as_ref().err());
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

fn steps(r: &mut Run) -> Result<(), String> {
    users(r)?;
    directories(r)?;
    master_key(r)?;
    db_url(r)?;
    config(r)?;
    let db_ready = database(r)?;
    systemd(r)?;
    sshd(r)?;
    if !db_ready {
        r.manual(
            "migrate",
            "after the database exists, run `slath setup` again",
        );
        return Ok(());
    }
    if r.opts.check && r.users.is_none() {
        r.change("checks", "run once the files above exist");
        return Ok(());
    }
    checks(r)?;
    let fresh = migrate(r)?;
    admin(r, fresh)?;
    start(r)
}

fn report(r: &Run, error: Option<&String>) {
    let changes = r.entries.iter().filter(|e| e.kind == Kind::Change).count();
    let manual = r.entries.iter().filter(|e| e.kind == Kind::Manual).count();
    match r.opts.output {
        OutputFormat::Text => {
            for e in &r.entries {
                let mark = match (e.kind, r.opts.check) {
                    (Kind::Ok, _) => "ok     ",
                    (Kind::Change, true) => "would  ",
                    (Kind::Change, false) => "changed",
                    (Kind::Manual, _) => "manual ",
                };
                println!("{mark} {:<12} {}", e.step, e.detail);
            }
            if let Some(e) = error {
                println!("FAILED: {e}");
            } else if r.opts.check {
                println!(
                    "{changes} change(s) to make, {manual} manual step(s); nothing was changed."
                );
            } else if changes == 0 && manual == 0 {
                println!("Nothing to change.");
            } else {
                println!("{changes} change(s) made, {manual} manual step(s).");
            }
        }
        OutputFormat::Json => print_json(&json!({
            "ok": error.is_none(),
            "check": r.opts.check,
            "changes": changes,
            "manual": manual,
            "steps": r.entries,
            "error": error,
        })),
    }
}

// --- users ---------------------------------------------------------------

fn lookup() -> Option<(nix::unistd::User, nix::unistd::User, nix::unistd::Group)> {
    use nix::unistd::{Group, User};
    let slath = User::from_name("slath").ok()??;
    let git = User::from_name("git").ok()??;
    let git_group = Group::from_name("git").ok()??;
    Some((slath, git, git_group))
}

fn users(r: &mut Run) -> Result<(), String> {
    let file_ok = std::fs::read_to_string(&r.paths.sysusers).is_ok_and(|s| s == SYSUSERS);
    if !file_ok {
        r.change("users", format!("write {}", r.paths.sysusers.display()));
        if r.apply() {
            if let Some(dir) = r.paths.sysusers.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            }
            write_file(&r.paths.sysusers, SYSUSERS, 0o644, None)?;
        }
    }
    let complete = |l: &Option<(nix::unistd::User, nix::unistd::User, nix::unistd::Group)>| {
        l.as_ref()
            .is_some_and(|(_, _, g)| g.mem.iter().any(|m| m == "slath"))
    };
    let mut found = lookup();
    if !complete(&found) {
        r.change(
            "users",
            "create the service account `slath` and the SSH login `git` (systemd-sysusers)",
        );
        if r.apply() {
            system::sysusers(&r.paths.sysusers)?;
            found = lookup();
            if !complete(&found) {
                return Err("systemd-sysusers didn't create the users `slath` and `git`".into());
            }
        }
    }
    let Some((slath, git, git_group)) = found else {
        return Ok(());
    };
    // The SSH login stays apart from the service account.
    if git.uid == slath.uid || git.gid == slath.gid {
        return Err("`git` shares the UID or group of `slath`; give it a user of its own (see docs/admin/install.md)".into());
    }
    if nix::unistd::Group::from_gid(slath.gid)
        .ok()
        .flatten()
        .is_some_and(|g| g.mem.iter().any(|m| m == "git"))
    {
        return Err(
            "`git` is a member of group `slath`; remove it (`gpasswd -d git slath`)".into(),
        );
    }
    match shadow_locked("git") {
        Some(true) => {
            r.change("users", "allow key logins for `git` (no password)");
            if r.apply() {
                system::unlock_for_keys("git")?;
            }
        }
        Some(false) => {}
        None if r.opts.check => r.ok(
            "users",
            "can't read /etc/shadow without root; `git`'s login state unknown",
        ),
        None => return Err("cannot read /etc/shadow".into()),
    }
    let _ = git_group;
    r.users = Some(Ids {
        slath_uid: slath.uid.as_raw(),
        slath_gid: slath.gid.as_raw(),
    });
    if !r
        .entries
        .iter()
        .any(|e| e.step == "users" && e.kind == Kind::Change)
    {
        r.ok(
            "users",
            "`slath` and `git` exist; `slath` is in group `git`",
        );
    }
    Ok(())
}

/// Whether `user`'s password field locks it (`!…`). `None`: unreadable.
fn shadow_locked(user: &str) -> Option<bool> {
    let text = std::fs::read_to_string("/etc/shadow").ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(user)?.strip_prefix(':'))
        .map(|rest| rest.starts_with('!'))
}

// --- files -----------------------------------------------------------------

/// Writes `text` to `path` with `mode` and, if given, `uid:gid`.
fn write_file(path: &Path, text: &str, mode: u32, owner: Option<(u32, u32)>) -> Result<(), String> {
    let tmp = path.with_extension("slath-setup-tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        f.set_permissions(std::fs::Permissions::from_mode(mode))
            .map_err(|e| format!("cannot set the mode of {}: {e}", tmp.display()))?;
        if let Some((u, g)) = owner {
            std::os::unix::fs::fchown(&f, Some(u), Some(g))
                .map_err(|e| format!("cannot set the owner of {}: {e}", tmp.display()))?;
        }
        f.write_all(text.as_bytes())
            .and_then(|()| f.sync_all())
            .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Ensures owner and mode of an existing path; reports what it repaired.
fn ensure_attrs(
    r: &mut Run,
    step: &'static str,
    path: &Path,
    owner: Option<(u32, u32)>,
    mode: u32,
) -> Result<bool, String> {
    let m = std::fs::symlink_metadata(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if m.file_type().is_symlink() {
        return Err(format!(
            "{} is a symbolic link; replace it with the real file",
            path.display()
        ));
    }
    let mut fixed = false;
    if let Some((u, g)) = owner
        && (m.uid() != u || m.gid() != g)
    {
        r.change(
            step,
            format!(
                "set the owner of {} to {u}:{g} (was {}:{})",
                path.display(),
                m.uid(),
                m.gid()
            ),
        );
        if r.apply() {
            std::os::unix::fs::lchown(path, Some(u), Some(g))
                .map_err(|e| format!("cannot set the owner of {}: {e}", path.display()))?;
        }
        fixed = true;
    }
    if m.mode() & 0o7777 != mode {
        r.change(
            step,
            format!(
                "set the mode of {} to {mode:04o} (was {:04o})",
                path.display(),
                m.mode() & 0o7777
            ),
        );
        if r.apply() {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .map_err(|e| format!("cannot set the mode of {}: {e}", path.display()))?;
        }
        fixed = true;
    }
    Ok(fixed)
}

fn root_slath(r: &Run) -> Option<(u32, u32)> {
    r.users.map(|i| (0, i.slath_gid))
}

fn slath_slath(r: &Run) -> Option<(u32, u32)> {
    r.users.map(|i| (i.slath_uid, i.slath_gid))
}

fn directories(r: &mut Run) -> Result<(), String> {
    let wanted = [
        (r.paths.etc.clone(), root_slath(r), 0o750),
        (r.paths.data.clone(), slath_slath(r), 0o750),
        (r.paths.data.join("git-home"), slath_slath(r), 0o750),
    ];
    let mut changed = false;
    for (dir, owner, mode) in wanted {
        if !dir.is_dir() {
            r.change(
                "directories",
                format!("create {} ({mode:04o})", dir.display()),
            );
            changed = true;
            if r.apply() {
                std::fs::create_dir_all(&dir)
                    .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
                ensure_quiet(&dir, owner, mode)?;
            }
        } else {
            changed |= ensure_attrs(r, "directories", &dir, owner, mode)?;
        }
    }
    if !changed {
        r.ok(
            "directories",
            format!("{} and {}", r.paths.etc.display(), r.paths.data.display()),
        );
    }
    Ok(())
}

/// Sets owner and mode without reporting (for something just created).
fn ensure_quiet(path: &Path, owner: Option<(u32, u32)>, mode: u32) -> Result<(), String> {
    if let Some((u, g)) = owner {
        std::os::unix::fs::lchown(path, Some(u), Some(g))
            .map_err(|e| format!("cannot set the owner of {}: {e}", path.display()))?;
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| format!("cannot set the mode of {}: {e}", path.display()))
}

fn master_key(r: &mut Run) -> Result<(), String> {
    let path = r.paths.master_key();
    if path.exists() {
        if !ensure_attrs(r, "master-key", &path, slath_slath(r), 0o600)? {
            r.ok("master-key", path.display().to_string());
        }
        return Ok(());
    }
    r.change(
        "master-key",
        format!(
            "generate {}; back it up apart from your backups",
            path.display()
        ),
    );
    if r.apply() {
        slath_auth::MasterKey::load_or_create(&path).map_err(|e| e.to_string())?;
        ensure_quiet(&path, slath_slath(r), 0o600)?;
    }
    Ok(())
}

fn random_password() -> Result<String, String> {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let mut out = String::with_capacity(32);
    while out.len() < 32 {
        let bytes = slath_auth::token::random_bytes::<64>().map_err(|e| e.to_string())?;
        for b in bytes {
            // 224 = 4 × 56 keeps every character uniform.
            if b < 224 && out.len() < 32 {
                out.push(char::from(ALPHABET[usize::from(b) % ALPHABET.len()]));
            }
        }
    }
    Ok(out)
}

fn db_url(r: &mut Run) -> Result<(), String> {
    let path = r.paths.db_url();
    if path.exists() {
        if !ensure_attrs(r, "database-url", &path, root_slath(r), 0o640)? {
            r.ok("database-url", path.display().to_string());
        }
        return Ok(());
    }
    r.change(
        "database-url",
        format!(
            "write {} with a new random database password",
            path.display()
        ),
    );
    if r.apply() {
        let url = format!("postgres://slath:{}@localhost/slath", random_password()?);
        write_file(&path, &url, 0o640, root_slath(r))?;
    }
    Ok(())
}

/// The configuration a new installation starts with.
fn config_text(public_url: &str, listen: &str, ssh_port: u16, git: &Path, paths: &Paths) -> String {
    format!(
        "# Written by `slath setup`. Reference: deploy/slath.example.toml and\n\
         # docs/admin/install.md. `slath setup` never rewrites this file.\n\n\
         [server]\npublic_url = {public_url:?}\nlisten = {listen:?}\n\n\
         [database]\nurl_file = {db:?}\n\n\
         [storage]\ndata_dir = {data:?}\nrun_dir = \"/run/slath\"\n\
         # The SSH login's group: it may connect to the IPC socket, nothing else.\n\
         socket_group = \"git\"\n\n\
         [secrets]\nmaster_key_file = {key:?}\n\n\
         [git]\nbinary = {git:?}\nssh_user = \"git\"\nssh_port = {ssh_port}\n\n\
         [log]\nformat = \"json\"\nlevel = \"info\"\n",
        db = paths.db_url().display().to_string(),
        data = paths.data.display().to_string(),
        key = paths.master_key().display().to_string(),
        git = git.display().to_string(),
    )
}

/// The first `git` in the usual `PATH` order.
fn find_git() -> PathBuf {
    ["/usr/local/bin/git", "/usr/bin/git", "/bin/git"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("/usr/bin/git"))
}

fn prompt(question: &str) -> Option<String> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    print!("{question} ");
    std::io::stdout().flush().ok()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok()?;
    let line = line.trim().to_owned();
    (!line.is_empty()).then_some(line)
}

fn config(r: &mut Run) -> Result<(), String> {
    let path = r.paths.config();
    if path.exists() {
        if !ensure_attrs(r, "config", &path, root_slath(r), 0o640)? {
            r.ok("config", format!("{} (kept as it is)", path.display()));
        }
        return Ok(());
    }
    let url = match r.opts.public_url.clone() {
        Some(u) => u,
        None if r.opts.check => {
            r.change(
                "config",
                format!("write {} (needs --public-url)", path.display()),
            );
            return Ok(());
        }
        None => prompt("Public URL (how people reach Slath, e.g. https://git.example.org):")
            .ok_or("--public-url is required when setup can't ask")?,
    };
    slath_core::config::PublicUrl::parse(&url).map_err(|e| format!("--public-url: {e}"))?;
    r.change("config", format!("write {} for {url}", path.display()));
    if r.apply() {
        let text = config_text(&url, &r.opts.listen, r.opts.ssh_port, &find_git(), &r.paths);
        write_file(&path, &text, 0o640, root_slath(r))?;
    }
    Ok(())
}

// --- database ----------------------------------------------------------------

/// What the database helper does, as the `postgres` OS user.
#[derive(Serialize, Deserialize)]
struct DbRequest {
    role: String,
    password: String,
    database: String,
}

fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn quote_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The statements that create the role and the database.
fn db_statements(q: &DbRequest) -> [String; 2] {
    [
        format!(
            "CREATE ROLE {} LOGIN PASSWORD {}",
            quote_ident(&q.role),
            quote_literal(&q.password)
        ),
        format!(
            "CREATE DATABASE {} OWNER {}",
            quote_ident(&q.database),
            quote_ident(&q.role)
        ),
    ]
}

async fn create_role_and_db(conn: &mut sqlx::PgConnection, q: &DbRequest) -> Result<(), String> {
    use sqlx::Executor;
    let [role, db] = db_statements(q);
    let has_role: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)")
            .bind(&q.role)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| e.to_string())?;
    if has_role {
        let alter = format!(
            "ALTER ROLE {} LOGIN PASSWORD {}",
            quote_ident(&q.role),
            quote_literal(&q.password)
        );
        conn.execute(sqlx::AssertSqlSafe(alter))
            .await
            .map_err(|e| e.to_string())?;
    } else {
        conn.execute(sqlx::AssertSqlSafe(role))
            .await
            .map_err(|e| e.to_string())?;
    }
    let has_db: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(&q.database)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| e.to_string())?;
    if !has_db {
        conn.execute(sqlx::AssertSqlSafe(db))
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `slath setup db-helper`: runs as the `postgres` OS user and creates the
/// role and database through the local socket (peer authentication).
pub fn db_helper() -> ExitCode {
    let mut input = String::new();
    let result = (|| {
        std::io::stdin()
            .read_to_string(&mut input)
            .map_err(|e| e.to_string())?;
        let q: DbRequest = serde_json::from_str(&input).map_err(|e| e.to_string())?;
        runtime()?.block_on(async {
            use sqlx::Connection;
            let mut last = String::from("no PostgreSQL socket found");
            for dir in ["/run/postgresql", "/var/run/postgresql", "/tmp"] {
                if !Path::new(dir).join(".s.PGSQL.5432").exists() {
                    continue;
                }
                let opts = sqlx::postgres::PgConnectOptions::new()
                    .socket(dir)
                    .username("postgres")
                    .database("postgres");
                match sqlx::PgConnection::connect_with(&opts).await {
                    Ok(mut c) => return create_role_and_db(&mut c, &q).await,
                    Err(e) => last = e.to_string(),
                }
            }
            Err(last)
        })
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// Role, password, and database name from the URL file.
fn parse_db_url(text: &str) -> Result<DbRequest, String> {
    let u =
        url::Url::parse(text.trim()).map_err(|_| "the database URL file isn't a URL".to_owned())?;
    let decode = |s: &str| {
        percent_encoding::percent_decode_str(s)
            .decode_utf8_lossy()
            .into_owned()
    };
    Ok(DbRequest {
        role: decode(u.username()),
        password: decode(u.password().unwrap_or_default()),
        database: decode(u.path().trim_start_matches('/')),
    })
}

/// Returns whether the database is reachable with the URL file.
fn database(r: &mut Run) -> Result<bool, String> {
    let Ok(text) = std::fs::read_to_string(r.paths.db_url()) else {
        r.change(
            "database",
            "create role `slath` and database `slath`, once the URL file exists",
        );
        return Ok(false);
    };
    let rt = runtime()?;
    let url = Secret::new(text.trim().to_owned());
    let reachable = |url: &Secret<String>| {
        let mut pc = slath_db::PoolConfig::new(Secret::new(url.expose().clone()));
        pc.max_connections = 1;
        pc.acquire_timeout = std::time::Duration::from_secs(5);
        rt.block_on(async { slath_db::connect(&pc).await.map(drop) })
    };
    if reachable(&url).is_ok() {
        r.ok("database", "connects with the URL file");
        return Ok(true);
    }
    let q = parse_db_url(url.expose())?;
    if r.opts.check {
        r.change(
            "database",
            format!("create role {} and database {}", q.role, q.database),
        );
        return Ok(false);
    }
    let created = if let Some(admin) = &r.opts.pg_admin_url {
        rt.block_on(async {
            use sqlx::Connection;
            let mut c = sqlx::PgConnection::connect(admin)
                .await
                .map_err(|e| e.to_string())?;
            create_role_and_db(&mut c, &q).await
        })
    } else if let Ok(Some(pg)) = nix::unistd::User::from_name("postgres") {
        let req = serde_json::to_vec(&q).map_err(|e| e.to_string())?;
        system::db_helper(pg.uid.as_raw(), pg.gid.as_raw(), &req)
    } else {
        Err("there's no `postgres` user and no --pg-admin-url".to_owned())
    };
    match created.and_then(|()| reachable(&url).map_err(|e| e.to_string())) {
        Ok(()) => {
            r.change(
                "database",
                format!("created role {} and database {}", q.role, q.database),
            );
            Ok(true)
        }
        Err(e) => {
            let [a, b] = db_statements(&DbRequest {
                password: "<password from /etc/slath/db-url>".into(),
                ..q
            });
            r.manual(
                "database",
                format!("couldn't create it ({e}). As a PostgreSQL superuser, run: {a}; {b}; then run `slath setup` again"),
            );
            Ok(false)
        }
    }
}

// --- systemd and sshd ----------------------------------------------------------

fn unit_text(binary: &Path) -> String {
    UNIT.replace(
        "ExecStart=/usr/bin/slath serve",
        &format!("ExecStart={} serve", binary.display()),
    )
}

fn sshd_text(binary: &Path) -> String {
    SSHD.replace(
        "/usr/bin/slath ssh-keys",
        &format!("{} ssh-keys", binary.display()),
    )
}

/// sshd runs the key lookup only from a root-owned binary that nobody else
/// can write, in a directory chain likewise.
fn binary_is_safe(binary: &Path) -> bool {
    binary
        .ancestors()
        .all(|p| std::fs::metadata(p).is_ok_and(|m| m.uid() == 0 && m.mode() & 0o022 == 0))
}

fn install_text(r: &mut Run, step: &'static str, path: &Path, text: &str) -> Result<bool, String> {
    if std::fs::read_to_string(path).is_ok_and(|s| s == text) {
        return Ok(false);
    }
    r.change(step, format!("write {}", path.display()));
    if r.apply() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        write_file(path, text, 0o644, Some((0, 0)))?;
    }
    Ok(true)
}

fn systemd(r: &mut Run) -> Result<(), String> {
    if !binary_is_safe(&r.binary) {
        r.manual(
            "systemd",
            format!(
                "{} must belong to root and be writable by nobody else (sshd requires it): \
                 install -m 0755 {} /usr/local/bin/slath, then run /usr/local/bin/slath setup",
                r.binary.display(),
                r.binary.display()
            ),
        );
    }
    let unit = r.paths.unit.clone();
    let changed = install_text(r, "systemd", &unit, &unit_text(&r.binary))?;
    if changed && r.apply() {
        if system::systemd_running() {
            system::systemctl(&["daemon-reload"])?;
        }
    } else if !changed {
        r.ok("systemd", unit.display().to_string());
    }
    Ok(())
}

fn sshd(r: &mut Run) -> Result<(), String> {
    let path = r.paths.sshd();
    if !install_text(r, "sshd", &path, &sshd_text(&r.binary))? {
        r.ok("sshd", path.display().to_string());
    }
    let main = std::fs::read_to_string("/etc/ssh/sshd_config").unwrap_or_default();
    if !main
        .lines()
        .any(|l| l.trim_start().starts_with("Include") && l.contains("sshd_config.d"))
    {
        r.manual("sshd", "/etc/ssh/sshd_config doesn't include sshd_config.d/*.conf; add `Include /etc/ssh/sshd_config.d/*.conf` at its top");
    }
    Ok(())
}

// --- checks, schema, administrator, start ------------------------------------------

fn checks(r: &mut Run) -> Result<(), String> {
    let cfg = crate::config::load(&r.paths.config())?;
    let failed: Vec<String> = crate::check::run_checks(&cfg)?
        .into_iter()
        .filter(|(_, ok, _)| !ok)
        .map(|(name, _, detail)| format!("{name}: {detail}"))
        .collect();
    if failed.is_empty() {
        r.ok(
            "checks",
            "`slath config check` passes; group `git` can't read Slath's files",
        );
        Ok(())
    } else {
        Err(format!("configuration check failed: {}", failed.join("; ")))
    }
}

/// Returns whether the database was new.
fn migrate(r: &mut Run) -> Result<bool, String> {
    let cfg = crate::config::load(&r.paths.config())?;
    let url = crate::config::database_url(&cfg)?;
    let check = r.opts.check;
    let (schema, pending) = runtime()?.block_on(async {
        let pool = slath_db::connect(&slath_db::PoolConfig::new(url))
            .await
            .map_err(|e| e.to_string())?;
        let schema = slath_db::schema_version(&pool)
            .await
            .map_err(|e| e.to_string())?
            .unwrap_or(0);
        let pending = slath_db::pending_migrations(&pool)
            .await
            .map_err(|e| e.to_string())?
            .len();
        if schema == 0 && !check {
            slath_db::migrate(&pool).await.map_err(|e| e.to_string())?;
        }
        Ok::<_, String>((schema, pending))
    })?;
    match (schema, pending) {
        (_, 0) => r.ok("migrate", format!("schema {schema} is current")),
        (0, _) => r.change("migrate", "set up the database schema"),
        _ => r.manual("migrate", "the database is older than this version: restart the service to update it (it backs up first)"),
    }
    Ok(schema == 0)
}

fn admin(r: &mut Run, fresh: bool) -> Result<(), String> {
    let config = r.paths.config();
    let existing: i64 = if fresh && r.opts.check {
        0
    } else {
        let cfg = crate::config::load(&config)?;
        let url = crate::config::database_url(&cfg)?;
        runtime()?.block_on(async {
            let pool = slath_db::connect(&slath_db::PoolConfig::new(url))
                .await
                .map_err(|e| e.to_string())?;
            sqlx::query_scalar("SELECT count(*) FROM users WHERE is_admin AND state = 'active'")
                .fetch_one(&pool)
                .await
                .map_err(|e| e.to_string())
        })?
    };
    if existing > 0 {
        r.ok("admin", format!("{existing} site administrator(s)"));
        return Ok(());
    }
    let username = r.opts.admin_username.clone().or_else(|| {
        (!r.opts.check)
            .then(|| prompt("First administrator's user name (not admin or root):"))
            .flatten()
    });
    let email = r.opts.admin_email.clone().or_else(|| {
        (!r.opts.check && username.is_some())
            .then(|| prompt("Their email address:"))
            .flatten()
    });
    let Some(username) = username else {
        r.manual("admin", "create the first administrator: slath admin create-user --admin --username NAME --email ADDRESS");
        return Ok(());
    };
    r.change("admin", format!("create the site administrator {username}"));
    if !r.apply() {
        return Ok(());
    }
    let password = random_password()?;
    runtime()?.block_on(async {
        let svc = crate::admin::services(&config).await?;
        svc.create_account(
            &slath_core::authz::Subject::System,
            slath_service::accounts::NewAccount {
                username: username.clone(),
                email,
                display_name: None,
                password: Some(Secret::new(password.clone())),
                is_admin: true,
            },
        )
        .await
        .map(|_| ())
        .map_err(|e| crate::admin::describe(&e))
    })?;
    r.change(
        "admin",
        format!("password of {username}: {password} (shown once; change it after signing in)"),
    );
    Ok(())
}

fn start(r: &mut Run) -> Result<(), String> {
    if !system::systemd_running() {
        r.manual("start", "systemd isn't running here: start `slath serve` as user slath and reload sshd yourself");
        return Ok(());
    }
    if r.opts.check {
        r.change("start", "enable and start slath.service, reload sshd");
        return Ok(());
    }
    system::systemctl(&["enable", "--now", "slath.service"])?;
    system::reload_sshd()?;
    r.ok(
        "start",
        "slath.service is enabled and running; sshd reloaded",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_files_are_adapted_to_the_binary() {
        let b = Path::new("/usr/local/bin/slath");
        assert!(unit_text(b).contains("ExecStart=/usr/local/bin/slath serve"));
        assert!(sshd_text(b).contains("AuthorizedKeysCommand /usr/local/bin/slath ssh-keys"));
        assert!(SYSUSERS.contains("m slath git"));
    }

    #[test]
    fn the_written_config_loads() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            sysusers: dir.path().join("s"),
            etc: dir.path().join("etc"),
            data: dir.path().join("data"),
            unit: dir.path().join("u"),
            sshd_dir: dir.path().join("sshd"),
        };
        let text = config_text(
            "https://git.example.org",
            "127.0.0.1:3000",
            2222,
            Path::new("/usr/bin/git"),
            &paths,
        );
        let file = dir.path().join("slath.toml");
        std::fs::write(&file, text).unwrap();
        let cfg = crate::config::load(&file).unwrap();
        assert_eq!(cfg.git.ssh_port, 2222);
        assert_eq!(cfg.storage.socket_group.as_deref(), Some("git"));
        assert!(cfg.warnings.is_empty());
    }

    #[test]
    fn sql_is_quoted() {
        let [a, b] = db_statements(&DbRequest {
            role: "sl\"ath".into(),
            password: "it's".into(),
            database: "db".into(),
        });
        assert_eq!(a, "CREATE ROLE \"sl\"\"ath\" LOGIN PASSWORD 'it''s'");
        assert_eq!(b, "CREATE DATABASE \"db\" OWNER \"sl\"\"ath\"");
        let q = parse_db_url("postgres://slath:p%40ss@localhost/slath\n").unwrap();
        assert_eq!(
            (q.role.as_str(), q.password.as_str(), q.database.as_str()),
            ("slath", "p@ss", "slath")
        );
    }

    #[test]
    fn passwords_are_random_and_url_safe() {
        let a = random_password().unwrap();
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|b| b.is_ascii_alphanumeric()));
        assert_ne!(a, random_password().unwrap());
    }
}
