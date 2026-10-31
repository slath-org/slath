// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Git over SSH through a real sshd with the shipped config: only authorized
//! Git transport on the intended repository works; everything else is denied.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use slath_tests::fixtures;
use slath_tests::server::{DEMO, GitClient, TestRepo, TestServer};
use slath_tests::sshd::{self, Sshd};

const OTHER: TestRepo = TestRepo {
    id: "0192f1a2-3b4c-7d5e-8f60-000000000002",
    namespace: "bob",
    name: "other",
};

struct Env {
    // Field order matters: stop sshd before the server and its temp dir go away.
    sshd: Sshd,
    server: TestServer,
    keys: tempfile::TempDir,
    key: PathBuf,
    git: GitClient,
}

impl Env {
    fn ssh(&self, key: &Path) -> Command {
        let mut c = Command::new("ssh");
        c.args(self.sshd.ssh_args(key)).stdin(Stdio::null());
        c
    }

    /// Runs `ssh git@host <args>` with the authorized key; returns (success, stderr).
    fn ssh_run(&self, args: &[&str]) -> (bool, String) {
        let out = self
            .ssh(&self.key)
            .arg("git@127.0.0.1")
            .args(args)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        self.git
            .cmd(cwd)
            .env("GIT_SSH_COMMAND", self.sshd.git_ssh_command(&self.key))
            .args(args)
            .output()
            .unwrap()
    }
}

fn setup() -> Option<Env> {
    if sshd::skipped() {
        eprintln!("SLATH_SKIP_SSH_TESTS=1: skipping");
        return None;
    }
    let keys = tempfile::tempdir().unwrap();
    let (key, public) = sshd::keygen(keys.path(), "alice");
    let extra = format!(
        "[[spike.ssh_keys]]\nid = \"0192f1a2-3b4c-7d5e-8f60-0000000000aa\"\npublic_key = \"{public}\"\nrepos = [\"alice/demo\"]\n"
    );
    let server = TestServer::start_full(&[DEMO, OTHER], &extra);
    let sshd = Sshd::start(&server);
    Some(Env {
        sshd,
        server,
        keys,
        key,
        git: GitClient::new(),
    })
}

fn assert_denied(env: &Env, args: &[&str], expect: &str) {
    let (ok, stderr) = env.ssh_run(args);
    assert!(!ok, "{args:?} must be denied");
    assert!(
        stderr.contains(expect),
        "{args:?}: expected {expect:?} in:\n{stderr}\n{}",
        env.sshd.logs()
    );
}

#[test]
fn conf_git_02_authorized_clone_and_push() {
    let Some(env) = setup() else { return };
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    fixtures::generate("basic", &src, &[]);
    let url = env.sshd.url("alice", "demo");
    for proto in ["0", "2"] {
        let out = env.git(
            &src,
            &[
                "-c",
                &format!("protocol.version={proto}"),
                "push",
                "--quiet",
                &url,
                "refs/heads/*:refs/heads/*",
            ],
        );
        assert!(
            out.status.success(),
            "push v{proto}: {}\n{}",
            String::from_utf8_lossy(&out.stderr),
            env.sshd.logs()
        );
    }
    let out = env.git(work.path(), &["clone", "--quiet", &url, "clone"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let refs = |p: &Path| {
        env.git.ok(
            p,
            &[
                "for-each-ref",
                "--format=%(objectname)",
                "refs/heads/main",
                "refs/remotes/origin/main",
            ],
        )
    };
    assert_eq!(
        refs(&work.path().join("clone")).lines().next(),
        refs(&src).lines().next(),
        "cloned main differs from pushed main"
    );
    env.git.ok(
        &env.server.repo_path(&DEMO),
        &["fsck", "--strict", "--no-dangling"],
    );
    // Reserved refs are refused through the same hooks as over HTTP.
    let out = env.git(&src, &["push", &url, "HEAD:refs/slath/x"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("reserved"));
}

#[test]
fn conf_git_02_shell_and_arbitrary_commands_are_denied() {
    let Some(env) = setup() else { return };
    assert_denied(&env, &[], "unsupported command");
    assert_denied(&env, &["ls"], "unsupported command");
    assert_denied(&env, &["sh -c id"], "unsupported command");
    assert_denied(
        &env,
        &["git-upload-pack 'alice/demo'; id"],
        "unsupported command",
    );
    assert_denied(&env, &["scp -t /tmp"], "unsupported command");
}

#[test]
fn conf_git_02_upload_archive_is_denied() {
    let Some(env) = setup() else { return };
    assert_denied(
        &env,
        &["git-upload-archive 'alice/demo.git'"],
        "git-upload-archive is not supported",
    );
    let work = tempfile::tempdir().unwrap();
    let out = env.git(
        work.path(),
        &[
            "archive",
            "--remote",
            &env.sshd.url("alice", "demo"),
            "HEAD",
        ],
    );
    assert!(!out.status.success());
}

#[test]
fn conf_git_02_path_traversal_is_denied() {
    let Some(env) = setup() else { return };
    for path in [
        "../../etc",
        "alice/../bob/other",
        "/srv/alice/demo",
        "alice/demo/../../x",
        "./alice/demo",
    ] {
        assert_denied(
            &env,
            &[&format!("git-upload-pack '{path}'")],
            "unsupported command",
        );
    }
}

#[test]
fn conf_git_02_other_repositories_are_not_reachable() {
    let Some(env) = setup() else { return };
    // Exists but the key may not use it; wrong namespace; doesn't exist.
    // All look the same.
    let mut answers = Vec::new();
    for path in ["bob/other", "bob/demo", "alice/other", "alice/nope"] {
        let (ok, stderr) = env.ssh_run(&[&format!("git-upload-pack '{path}'")]);
        assert!(!ok);
        assert!(stderr.contains("repository not found"), "{path}: {stderr}");
        answers.push(stderr);
    }
    assert!(answers.windows(2).all(|w| w[0] == w[1]), "{answers:?}");
    let work = tempfile::tempdir().unwrap();
    assert!(
        !env.git(work.path(), &["clone", &env.sshd.url("bob", "other"), "c"])
            .status
            .success()
    );
}

#[test]
fn conf_git_02_tty_is_denied() {
    let Some(env) = setup() else { return };
    let out = env
        .ssh(&env.key)
        .args(["-tt", "git@127.0.0.1"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("PTY allocation request failed"), "{stderr}");
    // sshd ran nothing but the forced command, without a TTY.
    let logs = env.sshd.logs();
    assert!(logs.contains("Starting session: forced-command"), "{logs}");
    assert!(!logs.contains("Starting session: shell"), "{logs}");
    assert!(!logs.to_lowercase().contains("allocated pty"), "{logs}");
}

#[test]
fn conf_git_02_port_forwarding_is_denied() {
    let Some(env) = setup() else { return };
    let local = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut fwd = env
        .ssh(&env.key)
        .args([
            "-N",
            "-L",
            &format!("{local}:127.0.0.1:22"),
            "git@127.0.0.1",
        ])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Wait for the local listener, then try to use the tunnel.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut conn = loop {
        if let Ok(c) = TcpStream::connect(("127.0.0.1", local)) {
            break c;
        }
        assert!(Instant::now() < deadline, "ssh -L never listened");
        std::thread::sleep(Duration::from_millis(100));
    };
    conn.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut buf = [0u8; 16];
    let n = conn.read(&mut buf).unwrap_or(0);
    assert_eq!(
        n,
        0,
        "forwarded connection reached the target: {:?}",
        &buf[..n]
    );
    let _ = fwd.kill();
    let mut stderr = String::new();
    fwd.stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let _ = fwd.wait();
    let logs = env.sshd.logs();
    assert!(
        logs.contains("refused local port forward"),
        "{stderr:?}\n{logs}"
    );
}

#[test]
fn conf_git_02_unknown_key_is_rejected() {
    let Some(env) = setup() else { return };
    let (stranger, _) = sshd::keygen(env.keys.path(), "stranger");
    let out = env
        .ssh(&stranger)
        .args(["git@127.0.0.1", "git-upload-pack 'alice/demo'"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Permission denied (publickey)"), "{stderr}");
}
