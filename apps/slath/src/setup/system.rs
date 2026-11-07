// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The system tools `slath setup` runs as root. This is the only module
//! outside the Git runner that starts processes (allowlisted in the guards):
//! fixed programs with fixed arguments, no shell, and no user input apart
//! from paths and names `setup` built itself.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// True if systemd manages this host (PID 1), so `systemctl` works.
#[must_use]
pub fn systemd_running() -> bool {
    Path::new("/run/systemd/system").is_dir()
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Creates the users and groups declared in a sysusers.d file.
pub fn sysusers(file: &Path) -> Result<(), String> {
    let f = file.to_str().ok_or("the sysusers path isn't UTF-8")?;
    run("systemd-sysusers", &[f])
}

/// Allows key logins for `user` without giving it a password.
pub fn unlock_for_keys(user: &str) -> Result<(), String> {
    run("usermod", &["--password", "*", user])
}

pub fn systemctl(args: &[&str]) -> Result<(), String> {
    run("systemctl", args)
}

/// Reloads sshd under whichever unit name the distribution uses.
pub fn reload_sshd() -> Result<(), String> {
    systemctl(&["reload", "ssh.service"]).or_else(|_| systemctl(&["reload", "sshd.service"]))
}

/// Runs `slath setup db-helper` as `uid`/`gid` (the `postgres` OS user, so
/// PostgreSQL's peer authentication lets it in as the superuser), with the
/// request on stdin. Returns the helper's error message on failure.
pub fn db_helper(uid: u32, gid: u32, request: &[u8]) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().map_err(|e| format!("cannot find the slath binary: {e}"))?;
    let mut child = Command::new(exe)
        .args(["setup", "db-helper"])
        .env_clear()
        .current_dir("/")
        .uid(uid)
        .gid(gid)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start the database helper: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(request)
            .map_err(|e| format!("cannot talk to the database helper: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("the database helper failed: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}
