// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Launches `slath serve` in a temporary data directory on a random port.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Password of every account the launcher creates.
pub const PASSWORD: &str = "slath test password";

/// Path of the `slath` binary: `$SLATH_BIN`, or next to the test's deps dir.
#[must_use]
pub fn slath_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("SLATH_BIN") {
        return PathBuf::from(p);
    }
    // Test binaries live in target/<profile>/deps, helper binaries in
    // target/<profile>; `slath` is in target/<profile>.
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap();
    let bin = [dir.join("slath"), dir.parent().unwrap().join("slath")]
        .into_iter()
        .find(|p| p.is_file())
        .unwrap_or_else(|| dir.join("slath"));
    assert!(
        bin.is_file(),
        "{} not found; build it with `cargo build -p slath`",
        bin.display()
    );
    bin
}

/// Git binary for both server and client: `$SLATH_TEST_GIT` or `/usr/bin/git`.
#[must_use]
pub fn git_bin() -> PathBuf {
    std::env::var_os("SLATH_TEST_GIT").map_or_else(|| PathBuf::from("/usr/bin/git"), PathBuf::from)
}

/// A repository the launcher creates (private, owned by the user named
/// like its namespace).
pub struct TestRepo {
    pub namespace: &'static str,
    pub name: &'static str,
}

/// The default repository of [`TestServer::start`].
pub const DEMO: TestRepo = TestRepo {
    namespace: "alice",
    name: "demo",
};

/// A fresh, empty PostgreSQL database for one test server, dropped again
/// when the value is dropped. The server connects with `$DATABASE_URL`'s
/// credentials (default: the `just dev-db` instance).
pub struct TestDb {
    pub url: String,
    admin_url: String,
    name: String,
}

fn admin_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://slath:slath@127.0.0.1:54329/slath".to_owned())
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

impl TestDb {
    #[must_use]
    pub fn create() -> Self {
        use sqlx::Connection;
        let admin_url = admin_url();
        let name = format!("slath_t_{}", uuid::Uuid::now_v7().simple());
        block_on(async {
            let mut c = sqlx::PgConnection::connect(&admin_url).await.unwrap();
            sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
                .execute(&mut c)
                .await
                .unwrap();
        });
        let base = admin_url
            .rsplit_once('/')
            .map_or(admin_url.as_str(), |(b, _)| b);
        Self {
            url: format!("{base}/{name}"),
            admin_url: admin_url.clone(),
            name,
        }
    }

    /// Runs SQL against this database (test setup and inspection).
    pub fn execute(&self, sql: &str) {
        Self::run(&self.url, sql);
    }

    /// Runs SQL on the maintenance connection (e.g. `ALTER DATABASE` on this one).
    pub fn execute_admin(&self, sql: &str) {
        Self::run(&self.admin_url, sql);
    }

    /// Runs a query returning one text value (first column of the first row).
    #[must_use]
    pub fn query_one(&self, sql: &str) -> String {
        use sqlx::{Connection, Row};
        block_on(async {
            let mut c = sqlx::PgConnection::connect(&self.url).await.unwrap();
            let row = sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
                .fetch_one(&mut c)
                .await
                .unwrap();
            row.try_get::<String, _>(0).unwrap()
        })
    }

    /// The database name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    fn run(url: &str, sql: &str) {
        use sqlx::Connection;
        block_on(async {
            let mut c = sqlx::PgConnection::connect(url).await.unwrap();
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_owned()))
                .execute(&mut c)
                .await
                .unwrap();
        });
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        use sqlx::Connection;
        let (url, name) = (self.admin_url.clone(), self.name.clone());
        let _ = std::thread::spawn(move || {
            block_on(async {
                if let Ok(mut c) = sqlx::PgConnection::connect(&url).await {
                    let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                        "DROP DATABASE IF EXISTS {name} WITH (FORCE)"
                    )))
                    .execute(&mut c)
                    .await;
                }
            });
        })
        .join();
    }
}

pub struct TestServer {
    child: Child,
    /// A token of `alice` with `repo:write` and `user:read`, if the server
    /// was started with repositories.
    pub token: String,
    pub db: TestDb,
    pub addr: SocketAddr,
    pub dir: tempfile::TempDir,
    /// The `slath` binary serving and running commands (an older release's
    /// in upgrade tests).
    pub bin: PathBuf,
}

impl TestServer {
    /// Starts a server hosting [`DEMO`].
    #[must_use]
    pub fn start() -> Self {
        Self::start_with(&[DEMO])
    }

    #[must_use]
    pub fn start_with(repos: &[TestRepo]) -> Self {
        Self::start_full(repos, "")
    }

    /// Starts a server hosting `repos`, with `extra` appended to the config
    /// (e.g. `[auth]` settings).
    #[must_use]
    pub fn start_full(repos: &[TestRepo], extra: &str) -> Self {
        Self::start_opts(repos, extra, &[])
    }

    /// Like [`TestServer::start_full`], with extra environment variables for
    /// the server (e.g. `SLATH__SERVER__PUBLIC_URL`).
    #[must_use]
    pub fn start_opts(repos: &[TestRepo], extra: &str, env: &[(&str, &str)]) -> Self {
        let mut server = Instance::new(extra).start(env);
        if !repos.is_empty() {
            server.seed(repos);
        }
        server
    }

    fn launch(dir: tempfile::TempDir, db: TestDb, env: &[(&str, &str)]) -> Self {
        Self::launch_bin(dir, db, env, slath_bin())
    }

    fn launch_bin(dir: tempfile::TempDir, db: TestDb, env: &[(&str, &str)], bin: PathBuf) -> Self {
        let cfg_path = dir.path().join("slath.toml");
        let mut attempt = 1;
        loop {
            match spawn_server(&bin, &cfg_path, &dir.path().join("server.log"), env, true) {
                Ok((child, addr)) => {
                    return Self {
                        child,
                        token: String::new(),
                        db,
                        addr,
                        dir,
                        bin,
                    };
                }
                // The free port picked earlier can be taken by another
                // process before the server binds it; move to a new port.
                Err(log) if log.contains("Address already in use") && attempt < 5 => {
                    let cfg = std::fs::read_to_string(&cfg_path).unwrap();
                    let old = cfg
                        .split("listen = \"127.0.0.1:")
                        .nth(1)
                        .and_then(|r| r.split('"').next())
                        .unwrap()
                        .to_owned();
                    let cfg = cfg.replace(
                        &format!("127.0.0.1:{old}"),
                        &format!("127.0.0.1:{}", free_port()),
                    );
                    std::fs::write(&cfg_path, cfg).unwrap();
                    attempt += 1;
                }
                Err(log) => panic!("slath serve did not start:\n{log}"),
            }
        }
    }

    /// Stops the server (its data stays).
    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Starts the stopped server again with `bin` (`serve`, plus `--migrate`
    /// if asked). Returns the server's log if it doesn't start.
    pub fn start_again(&mut self, bin: &Path, migrate: bool) -> Result<(), String> {
        self.stop();
        let (child, addr) = spawn_server(
            bin,
            &self.config_path(),
            &self.dir.path().join("server.log"),
            &[],
            migrate,
        )?;
        self.child = child;
        self.addr = addr;
        self.bin = bin.to_path_buf();
        Ok(())
    }

    /// Creates the repositories' owners (and `alice`), the repositories, and
    /// `alice`'s token.
    /// Creates repositories with their owners, `alice`, and her token.
    pub fn seed_repos(&mut self, repos: &[TestRepo]) {
        self.seed(repos);
    }

    fn seed(&mut self, repos: &[TestRepo]) {
        let mut users = vec!["alice"];
        users.extend(repos.iter().map(|r| r.namespace));
        users.sort_unstable();
        users.dedup();
        for u in &users {
            self.create_user(u, PASSWORD, false);
        }
        for r in repos {
            let mut c = self.login(r.namespace, PASSWORD);
            crate::http::create_repo(&mut c, r.namespace, r.name, "private");
        }
        let mut alice = self.login("alice", PASSWORD);
        self.token = crate::http::create_token(&mut alice, "tests", &["repo:write", "user:read"]);
    }

    /// Adds an SSH public key (one `authorized_keys` line) to a user.
    pub fn add_ssh_key(&self, user: &str, public_key: &str) {
        let mut c = self.login(user, PASSWORD);
        let r = crate::http::submit(
            &mut c,
            "/-/settings/keys",
            "/-/settings/keys",
            &[("title", "test key"), ("key", public_key)],
        );
        assert_eq!(r.status, 303, "adding the key failed: {}", r.body);
    }

    #[must_use]
    pub fn config_path(&self) -> PathBuf {
        self.dir.path().join("slath.toml")
    }

    /// Runs a `slath` subcommand against this server's config; returns
    /// (success, stdout, stderr).
    pub fn slath(&self, args: &[&str], stdin: Option<&str>) -> (bool, String, String) {
        run_slath_bin(&self.bin, &self.config_path(), args, stdin)
    }

    /// Creates a user through the CLI and returns its ID.
    pub fn create_user(&self, name: &str, password: &str, admin: bool) -> String {
        let mut args = vec!["admin", "create-user", "--username", name, "--email"];
        let email = format!("{name}@example.org");
        args.push(&email);
        args.extend(["--password-stdin", "--output", "json"]);
        if admin {
            args.push("--admin");
        }
        let (ok, out, err) = self.slath(&args, Some(password));
        assert!(ok, "create-user {name} failed: {out}{err}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        v["user"]["id"].as_str().unwrap().to_owned()
    }

    /// A client signed in as `name`.
    #[must_use]
    pub fn login(&self, name: &str, password: &str) -> crate::http::Client {
        let mut c = self.client();
        let r = crate::http::login(&mut c, name, password);
        assert_eq!(r.status, 303, "login as {name} failed: {}", r.body);
        c
    }

    /// Base URL, e.g. `http://127.0.0.1:41234`.
    #[must_use]
    pub fn base(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// A fresh HTTP client (empty cookie jar) for this server.
    #[must_use]
    pub fn client(&self) -> crate::http::Client {
        crate::http::Client::new(&self.base())
    }

    #[must_use]
    pub fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    #[must_use]
    pub fn ipc_socket(&self) -> PathBuf {
        self.dir.path().join("run").join("slath.sock")
    }

    /// Bare repository path of a test repo (ID-based layout).
    #[must_use]
    pub fn repo_path(&self, repo: &TestRepo) -> PathBuf {
        let id = self.db.query_one(&format!(
            "SELECT r.id::text FROM repositories r JOIN namespaces n ON n.id = r.namespace_id \
             WHERE n.name = '{}' AND r.name = '{}'",
            repo.namespace, repo.name
        ));
        self.data_dir()
            .join("repos")
            .join(&id[..2])
            .join(format!("{id}.git"))
    }

    /// Clone URL with credentials embedded.
    #[must_use]
    pub fn url(&self, repo: &TestRepo) -> String {
        format!(
            "http://git:{}@{}/{}/{}.git",
            self.token, self.addr, repo.namespace, repo.name
        )
    }

    #[must_use]
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("server.log")).unwrap_or_default()
    }

    /// Sends one raw HTTP/1.1 request and returns the full response bytes.
    #[must_use]
    pub fn raw_request(&self, request: &[u8]) -> Vec<u8> {
        let mut s = TcpStream::connect(self.addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        s.write_all(request).unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
        out
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts `slath serve` and waits for its "listening" line. On failure,
/// returns the log.
fn spawn_server(
    bin: &Path,
    cfg_path: &Path,
    log_path: &Path,
    env: &[(&str, &str)],
    migrate: bool,
) -> Result<(Child, SocketAddr), String> {
    let log = std::fs::File::create(log_path).unwrap();
    let mut cmd = Command::new(bin);
    cmd.arg("serve");
    if migrate {
        cmd.arg("--migrate");
    }
    let mut child = cmd
        .arg("--config")
        .arg(cfg_path)
        .envs(env.iter().copied())
        .stdout(Stdio::piped())
        .stderr(log)
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if let Some(url) = line.strip_prefix("slath: listening on http://") {
                let _ = tx.send(url.to_owned());
            }
        }
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(addr) => Ok((child, addr.parse().unwrap())),
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(std::fs::read_to_string(log_path).unwrap_or_default())
        }
    }
}

/// A currently free TCP port on the loopback interface. The server needs
/// its port before it starts, because `public_url` must name it.
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

/// A Git client with an isolated configuration, run in `cwd`.
pub struct GitClient {
    pub home: tempfile::TempDir,
}

impl Default for GitClient {
    fn default() -> Self {
        Self::new()
    }
}

impl GitClient {
    #[must_use]
    pub fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
        }
    }

    #[must_use]
    pub fn cmd(&self, cwd: &Path) -> Command {
        let mut c = Command::new(git_bin());
        c.current_dir(cwd)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.home.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.org",
                "-c",
                "init.defaultBranch=main",
            ]);
        c
    }

    /// Runs git and returns stdout; panics with stderr on failure.
    pub fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.cmd(cwd).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Runs git expecting failure; returns stderr.
    pub fn fails(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.cmd(cwd).args(args).output().unwrap();
        assert!(!out.status.success(), "git {args:?} unexpectedly succeeded");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }
}

/// Runs a `slath` subcommand with `config`; returns (success, stdout, stderr).
pub fn run_slath(config: &Path, args: &[&str], stdin: Option<&str>) -> (bool, String, String) {
    run_slath_bin(&slath_bin(), config, args, stdin)
}

/// Like [`run_slath`] with a specific binary.
pub fn run_slath_bin(
    bin: &Path,
    config: &Path,
    args: &[&str],
    stdin: Option<&str>,
) -> (bool, String, String) {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .env("SLATH_CONFIG", config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The IPC socket's group in tests: this process's group, which the sshd
/// fixture's `git` login (a different user) is a member of.
#[must_use]
pub fn socket_gid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    let probe = tempfile::tempfile().unwrap();
    probe.metadata().unwrap().gid()
}

/// A configured but not yet started instance: an empty database and data
/// directory (e.g. a restore target).
pub struct Instance {
    pub dir: tempfile::TempDir,
    pub db: TestDb,
}

impl Instance {
    #[must_use]
    pub fn new(extra: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        // Private, like a real installation's directories.
        std::fs::set_permissions(
            dir.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let db = TestDb::create();
        let port = free_port();
        // Administrators don't need a second factor here unless a test turns
        // it on (SLATH__AUTH__REQUIRE_MFA_FOR_ADMINS=true).
        let mut cfg = format!(
            "[server]\nlisten = \"127.0.0.1:{port}\"\npublic_url = \"http://127.0.0.1:{port}\"\nmetrics_listen = \"\"\n\
             [database]\nurl = \"{db_url}\"\nmax_connections = 5\n[storage]\ndata_dir = \"{data}\"\nrun_dir = \"{run}\"\n\
             socket_group = \"{gid}\"\n\
             [git]\nbinary = \"{git}\"\n[log]\nformat = \"pretty\"\nlevel = \"info\"\n\
             [auth]\nrequire_mfa_for_admins = false\n",
            gid = socket_gid(),
            data = dir.path().join("data").display(),
            run = dir.path().join("run").display(),
            git = git_bin().display(),
            db_url = db.url,
        );
        cfg += extra;
        std::fs::write(dir.path().join("slath.toml"), cfg).unwrap();
        Self { dir, db }
    }

    #[must_use]
    pub fn config_path(&self) -> PathBuf {
        self.dir.path().join("slath.toml")
    }

    /// Uses `http://localhost:<port>` as `public_url` (WebAuthn needs a
    /// host name, not an IP address). Returns the new origin.
    #[must_use]
    pub fn use_localhost(&self) -> String {
        let p = self.config_path();
        let cfg = std::fs::read_to_string(&p).unwrap();
        let cfg = cfg.replace(
            "public_url = \"http://127.0.0.1:",
            "public_url = \"http://localhost:",
        );
        std::fs::write(&p, &cfg).unwrap();
        let port = cfg
            .split("public_url = \"http://localhost:")
            .nth(1)
            .and_then(|r| r.split('"').next())
            .unwrap()
            .to_owned();
        format!("http://localhost:{port}")
    }

    pub fn slath(&self, args: &[&str], stdin: Option<&str>) -> (bool, String, String) {
        run_slath(&self.config_path(), args, stdin)
    }

    /// Starts `slath serve --migrate` on this instance.
    #[must_use]
    pub fn start(self, env: &[(&str, &str)]) -> TestServer {
        TestServer::launch(self.dir, self.db, env)
    }

    /// Starts `serve --migrate` of another binary (an older release).
    #[must_use]
    pub fn start_bin(self, bin: &Path, env: &[(&str, &str)]) -> TestServer {
        TestServer::launch_bin(self.dir, self.db, env, bin.to_path_buf())
    }
}
