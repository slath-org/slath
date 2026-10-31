// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! A real OpenSSH server in a container, configured with the shipped
//! `deploy/sshd/slath.conf`, in front of a [`TestServer`].
//!
//! The container shares the test server's temp directory (same absolute
//! path) so `ssh-serve` finds the IPC socket and repositories, and runs Git
//! as a user with the test runner's UID. Needs Docker (or a compatible CLI
//! via `$SLATH_CONTAINER_ENGINE`). Set `SLATH_SKIP_SSH_TESTS=1` to skip.

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::server::{TestServer, slath_bin};

fn engine() -> String {
    std::env::var("SLATH_CONTAINER_ENGINE").unwrap_or_else(|_| "docker".to_owned())
}

/// True if SSH tests were explicitly disabled.
#[must_use]
pub fn skipped() -> bool {
    std::env::var_os("SLATH_SKIP_SSH_TESTS").is_some_and(|v| v == "1")
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("compat")
        .join("ssh")
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{cmd:?} failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Builds the fixture image once per content hash and UID.
fn image() -> String {
    let dir = fixture_dir();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for f in ["Dockerfile", "sshd_config", "entrypoint.sh"] {
        for b in std::fs::read(dir.join(f)).unwrap() {
            hash = (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    let uid = run(Command::new("id").arg("-u"));
    let tag = format!("slath-test-sshd:{hash:016x}-{uid}");
    let exists = Command::new(engine())
        .args(["image", "inspect", &tag])
        .output()
        .unwrap();
    if !exists.status.success() {
        run(Command::new(engine())
            .args([
                "build",
                "--quiet",
                "--build-arg",
                &format!("UID={uid}"),
                "-t",
                &tag,
            ])
            .arg(&dir));
    }
    tag
}

pub struct Sshd {
    container: String,
    pub port: u16,
}

impl Sshd {
    #[must_use]
    pub fn start(server: &TestServer) -> Self {
        let shared = server.dir.path().to_str().unwrap().to_owned();
        let bin = slath_bin().canonicalize().unwrap();
        let bin = bin.to_str().unwrap();
        let conf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../deploy/sshd/slath.conf");
        let conf = conf.canonicalize().unwrap();
        let socket = server.ipc_socket();
        let container = run(Command::new(engine())
            .args(["run", "-d", "--rm", "-p", "127.0.0.1::22"])
            .args(["-v", &format!("{shared}:{shared}")])
            // Installed as root-owned /usr/bin/slath by the entrypoint.
            .args(["-v", &format!("{bin}:/mnt/slath:ro")])
            // Same path as on the host, for the managed hook symlinks.
            .args(["-v", &format!("{bin}:{bin}:ro")])
            .args([
                "-v",
                &format!("{}:/etc/ssh/sshd_config.d/slath.conf:ro", conf.display()),
            ])
            // The shipped config expects the socket at /run/slath/slath.sock.
            .args(["-v", &format!("{}:/run/slath/slath.sock", socket.display())])
            .arg(image()));
        let mapping = run(Command::new(engine()).args(["port", &container, "22/tcp"]));
        let port = mapping
            .lines()
            .find_map(|l| l.rsplit(':').next()?.parse().ok())
            .unwrap();
        let sshd = Self { container, port };
        sshd.wait_ready();
        sshd
    }

    fn wait_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Ok(mut s) = TcpStream::connect(("127.0.0.1", self.port)) {
                use std::io::Read;
                let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                let mut banner = [0u8; 4];
                if s.read_exact(&mut banner).is_ok() && &banner == b"SSH-" {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("sshd did not come up:\n{}", self.logs());
    }

    #[must_use]
    pub fn logs(&self) -> String {
        let out = Command::new(engine())
            .args(["logs", &self.container])
            .output()
            .unwrap();
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }

    /// `ssh` arguments for connecting as `git` with `key`, with host key checks off.
    #[must_use]
    pub fn ssh_args(&self, key: &Path) -> Vec<String> {
        vec![
            "-p".into(),
            self.port.to_string(),
            "-i".into(),
            key.display().to_string(),
            "-F".into(),
            "/dev/null".into(),
            "-o".into(),
            "IdentitiesOnly=yes".into(),
            "-o".into(),
            "StrictHostKeyChecking=no".into(),
            "-o".into(),
            "UserKnownHostsFile=/dev/null".into(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "LogLevel=ERROR".into(),
        ]
    }

    /// A `GIT_SSH_COMMAND` value using `key`.
    #[must_use]
    pub fn git_ssh_command(&self, key: &Path) -> String {
        format!("ssh {}", self.ssh_args(key).join(" "))
    }

    #[must_use]
    pub fn url(&self, namespace: &str, repo: &str) -> String {
        format!("ssh://git@127.0.0.1:{}/{namespace}/{repo}.git", self.port)
    }
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = Command::new(engine())
            .args(["rm", "-f", &self.container])
            .output();
    }
}

/// Generates an Ed25519 key pair in `dir`; returns (private key path, public key line).
#[must_use]
pub fn keygen(dir: &Path, name: &str) -> (PathBuf, String) {
    let path = dir.join(name);
    run(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", name, "-f"])
        .arg(&path));
    let public = std::fs::read_to_string(path.with_extension("pub")).unwrap();
    (path, public.trim().to_owned())
}
