// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath setup` on a clean host (OPS-10, SEC-01): a Debian container with
//! PostgreSQL and OpenSSH from the distribution plays the host
//! (tests/ops/setup-host, built by `scripts/test-setup.sh`). `--check`
//! changes nothing, setup yields a working instance (the OPS-01 flow), a
//! rerun changes nothing, and damaged permissions are repaired.

use std::process::Command;

use crate::compose::install_flow;

const ENGINE_DEFAULT: &str = "docker";

struct Host {
    engine: String,
    name: String,
}

impl Host {
    fn start(image: &str, http: u16, ssh: u16) -> Self {
        let engine =
            std::env::var("SLATH_CONTAINER_ENGINE").unwrap_or_else(|_| ENGINE_DEFAULT.into());
        let name = format!("slath-setup-{}", std::process::id());
        let out = Command::new(&engine)
            .args(["run", "-d", "--rm", "--name", &name])
            .args([
                "-p",
                &format!("127.0.0.1:{http}:3000"),
                "-p",
                &format!("127.0.0.1:{ssh}:22"),
            ])
            .arg(image)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Self { engine, name }
    }

    /// Runs `cmd` in the host as root; returns (success, stdout + stderr).
    fn sh(&self, cmd: &str) -> (bool, String) {
        let out = Command::new(&self.engine)
            .args(["exec", &self.name, "sh", "-c", cmd])
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    }

    fn ok(&self, cmd: &str) -> String {
        let (ok, out) = self.sh(cmd);
        assert!(ok, "{cmd}: {out}");
        out
    }

    /// Runs `slath setup` with `args` and returns its JSON report.
    fn setup(&self, args: &str) -> serde_json::Value {
        let (ok, out) = self.sh(&format!("/usr/local/bin/slath setup --output json {args}"));
        let v: serde_json::Value = serde_json::from_str(out.lines().last().unwrap_or_default())
            .unwrap_or_else(|_| panic!("setup {args}: {out}"));
        assert!(ok && v["ok"] == true, "setup {args}: {out}");
        v
    }

    /// Everything setup could have touched, as text.
    fn state(&self) -> String {
        self.ok(
            "getent passwd slath git || true; getent group git || true; \
             ls -lanR /etc/slath /var/lib/slath 2>&1 | grep -v '^total' || true; \
             ls -lan /etc/systemd/system /etc/ssh/sshd_config.d /usr/lib/sysusers.d 2>&1 || true; \
             su postgres -c \"psql -At -c 'SELECT rolname FROM pg_roles ORDER BY 1'\"",
        )
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = Command::new(&self.engine)
            .args(["rm", "-f", &self.name])
            .output();
    }
}

fn steps_with(v: &serde_json::Value, kind: &str) -> Vec<String> {
    v["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["kind"] == kind)
        .map(|s| {
            format!(
                "{}: {}",
                s["step"].as_str().unwrap(),
                s["detail"].as_str().unwrap()
            )
        })
        .collect()
}

#[test]
#[ignore = "needs the clean-host image; run scripts/test-setup.sh"]
fn conf_ops_10_setup_installs_reruns_cleanly_and_repairs() {
    let image = std::env::var("SLATH_SETUP_HOST_IMAGE").expect("run scripts/test-setup.sh");
    let http: u16 = std::env::var("SLATH_TEST_HTTP_PORT").map_or(38180, |p| p.parse().unwrap());
    let ssh: u16 = std::env::var("SLATH_TEST_SSH_PORT").map_or(38122, |p| p.parse().unwrap());
    let host = Host::start(&image, http, ssh);
    host.ok("pg_ctlcluster $(ls /etc/postgresql) main start");
    // The administrator downloads the binary and installs it root-owned.
    host.ok("install -m 0755 /root/slath /usr/local/bin/slath");
    let flags = format!(
        "--public-url http://127.0.0.1:{http} --listen 0.0.0.0:3000 --ssh-port {ssh} \
         --admin-username ada --admin-email ada@example.org"
    );

    // --check lists the changes and makes none.
    let before = host.state();
    let check = host.setup(&format!("--check {flags}"));
    assert!(check["changes"].as_u64().unwrap() >= 5, "{check}");
    assert_eq!(host.state(), before, "--check changed something");

    // Setup.
    let first = host.setup(&flags);
    let changed = steps_with(&first, "change");
    let password = changed
        .iter()
        .find_map(|s| s.strip_prefix("admin: password of ada: "))
        .and_then(|s| s.split(' ').next())
        .unwrap_or_else(|| panic!("no admin password in {first}"))
        .to_owned();
    // Without systemd as PID 1, starting is left to the operator.
    assert!(
        steps_with(&first, "manual")
            .iter()
            .any(|s| s.starts_with("start:")),
        "{first}"
    );

    // A rerun changes nothing.
    let second = host.setup(&flags);
    assert_eq!(second["changes"], 0, "{second}");

    // Damaged permissions are repaired, and only they.
    host.ok("chmod 0644 /etc/slath/master.key && chmod 0755 /var/lib/slath && chgrp root /etc/slath/db-url");
    let repair = host.setup(&flags);
    let fixed = steps_with(&repair, "change");
    assert_eq!(fixed.len(), 3, "{fixed:?}");
    assert!(fixed.iter().all(|f| f.contains("set the")), "{fixed:?}");
    assert_eq!(host.setup(&flags)["changes"], 0);
    let modes = host.ok("stat -c '%a %U:%G %n' /etc/slath /etc/slath/* /var/lib/slath");
    for want in [
        "750 root:slath /etc/slath\n",
        "600 slath:slath /etc/slath/master.key",
        "640 root:slath /etc/slath/db-url",
        "640 root:slath /etc/slath/slath.toml",
        "750 slath:slath /var/lib/slath",
    ] {
        assert!(modes.contains(want), "{want}: {modes}");
    }

    // SEC-01: the SSH login reads nothing of Slath's.
    for path in [
        "/etc/slath/master.key",
        "/etc/slath/db-url",
        "/etc/slath/slath.toml",
    ] {
        let (ok, _) = host.sh(&format!("su -s /bin/sh git -c 'cat {path}'"));
        assert!(!ok, "git can read {path}");
    }
    let (ok, _) = host.sh("su -s /bin/sh git -c 'ls /var/lib/slath'");
    assert!(!ok, "git can list the data directory");
    assert!(
        host.ok("id -nG slath")
            .split_whitespace()
            .any(|g| g == "git")
    );
    assert!(
        !host
            .ok("id -nG git")
            .split_whitespace()
            .any(|g| g == "slath")
    );

    // Start what systemd would start, then the OPS-01 flow.
    host.ok(
        "install -d -o slath -g slath -m 0700 /run/slath && mkdir -p /run/sshd && ssh-keygen -A",
    );
    host.ok("su -s /bin/sh slath -c 'SLATH_CONFIG=/etc/slath/slath.toml nohup /usr/local/bin/slath serve >/tmp/slath.log 2>&1 &'");
    host.ok("/usr/sbin/sshd");
    let base = format!("http://127.0.0.1:{http}");
    let start = std::time::Instant::now();
    while !host.sh("grep -q 'listening on' /tmp/slath.log").0 {
        assert!(
            start.elapsed().as_secs() < 60,
            "{}",
            host.ok("cat /tmp/slath.log")
        );
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert_eq!(
        slath_tests::http::Client::new(&base)
            .get("/-/readyz")
            .status,
        200
    );
    install_flow(&base, &ssh.to_string(), &password);
}
