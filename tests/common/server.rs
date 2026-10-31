// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Launches `slath serve` in a temporary data directory on a random port.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Token the spike server accepts (any non-empty username).
pub const TOKEN: &str = "slath-test-token-0123456789";

/// Path of the `slath` binary: `$SLATH_BIN`, or next to the test's deps dir.
#[must_use]
pub fn slath_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("SLATH_BIN") {
        return PathBuf::from(p);
    }
    let exe = std::env::current_exe().unwrap();
    let bin = exe.parent().and_then(Path::parent).unwrap().join("slath");
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

pub struct TestRepo {
    pub id: &'static str,
    pub namespace: &'static str,
    pub name: &'static str,
}

/// The default repository of [`TestServer::start`].
pub const DEMO: TestRepo = TestRepo {
    id: "0192f1a2-3b4c-7d5e-8f60-718293a4b5c6",
    namespace: "alice",
    name: "demo",
};

pub struct TestServer {
    child: Child,
    pub addr: SocketAddr,
    pub dir: tempfile::TempDir,
}

impl TestServer {
    /// Starts a server hosting [`DEMO`].
    #[must_use]
    pub fn start() -> Self {
        Self::start_with(&[DEMO])
    }

    #[must_use]
    pub fn start_with(repos: &[TestRepo]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = format!(
            "[server]\nlisten = \"127.0.0.1:0\"\n[storage]\ndata_dir = \"{data}\"\nrun_dir = \"{run}\"\n\
             [git]\nbinary = \"{git}\"\n[log]\nformat = \"pretty\"\nlevel = \"info\"\n\
             [spike]\ntoken = \"{TOKEN}\"\n",
            data = dir.path().join("data").display(),
            run = dir.path().join("run").display(),
            git = git_bin().display(),
        );
        for r in repos {
            cfg += &format!(
                "[[spike.repos]]\nid = \"{}\"\nnamespace = \"{}\"\nname = \"{}\"\n",
                r.id, r.namespace, r.name
            );
        }
        let cfg_path = dir.path().join("slath.toml");
        std::fs::write(&cfg_path, cfg).unwrap();
        let log = std::fs::File::create(dir.path().join("server.log")).unwrap();
        let mut child = Command::new(slath_bin())
            .args(["serve", "--config"])
            .arg(&cfg_path)
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
        let Ok(addr) = rx.recv_timeout(Duration::from_secs(30)) else {
            let _ = child.kill();
            let log = std::fs::read_to_string(dir.path().join("server.log")).unwrap_or_default();
            panic!("slath serve did not start:\n{log}");
        };
        Self {
            child,
            addr: addr.parse().unwrap(),
            dir,
        }
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
        self.data_dir()
            .join("repos")
            .join(&repo.id[..2])
            .join(format!("{}.git", repo.id))
    }

    /// Clone URL with credentials embedded.
    #[must_use]
    pub fn url(&self, repo: &TestRepo) -> String {
        format!(
            "http://git:{TOKEN}@{}/{}/{}.git",
            self.addr, repo.namespace, repo.name
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
