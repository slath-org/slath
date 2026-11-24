// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Push throughput: several Git clients push new commits to a real server
//! over smart HTTP for a while, and the report says how many pushes per
//! second the server took, how long each push took, and on what hardware.
//!
//! The regular test suite runs it briefly so it keeps working; the nightly
//! benchmark and `just bench-push` run it longer against a release build
//! (`SLATH_BIN`). Settings: `SLATH_BENCH_PUSH_CLIENTS` (default 4) and
//! `SLATH_BENCH_PUSH_SECONDS` (default 3). The report goes to
//! `target/perf-report/push-throughput.{json,md}`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;
use slath_tests::http::{create_repo, create_token};
use slath_tests::server::{GitClient, TestServer, git_bin, slath_bin};

const PW: &str = "correct horse battery staple";

fn setting(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&v| v > 0)
        .unwrap_or(default)
}

fn report_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/perf-report")
}

/// The first line of `path` that starts with `key`, after its colon.
fn proc_field(path: &str, key: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_owned())
}

fn hardware() -> serde_json::Value {
    let memory_kib: Option<u64> = proc_field("/proc/meminfo", "MemTotal")
        .and_then(|v| v.trim_end_matches("kB").trim().parse().ok());
    json!({
        "cpu": proc_field("/proc/cpuinfo", "model name"),
        "logical_cpus": std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
        "memory_mib": memory_kib.map(|k| k / 1024),
        "kernel": std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .map(|s| s.trim().to_owned()),
    })
}

fn git_output(git: &GitClient, cwd: &Path, args: &[&str]) -> Option<String> {
    let out = git.cmd(cwd).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The value at quantile `q` of sorted `v`.
fn quantile(v: &[f64], q: f64) -> f64 {
    let i = ((v.len() as f64 - 1.0) * q).round() as usize;
    v.get(i).copied().unwrap_or(0.0)
}

/// One client: pushes a new commit to its own branch until `deadline`;
/// returns each push's duration in milliseconds, or why a push failed.
fn client(url: &str, n: u64, deadline: Instant) -> Result<Vec<f64>, String> {
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    let branch = format!("bench/{n}");
    git.ok(w, &["init", "-q", "-b", &branch]);
    let mut times = Vec::new();
    let mut i = 0u64;
    while Instant::now() < deadline {
        i += 1;
        std::fs::write(w.join(format!("{n}.txt")), format!("{n} {i}\n")).unwrap();
        git.ok(w, &["add", "."]);
        git.ok(w, &["commit", "-q", "-m", &format!("push {i}")]);
        let start = Instant::now();
        let out = git
            .cmd(w)
            .args(["push", "-q", url, &branch])
            .output()
            .unwrap();
        let took = start.elapsed();
        if !out.status.success() {
            let at = time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default();
            return Err(format!(
                "push {i} of client {n} failed at {at}:\n{}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        times.push(took.as_secs_f64() * 1000.0);
    }
    Ok(times)
}

#[test]
fn conf_perf_04_push_throughput_is_measured() {
    let clients = setting("SLATH_BENCH_PUSH_CLIENTS", 4);
    let seconds = setting("SLATH_BENCH_PUSH_SECONDS", 3);
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    create_repo(&mut a, "alice", "bench", "private");
    let token = create_token(&mut a, "bench", &["repo:write"]);
    let url = format!(
        "http://git:{token}@{}/alice/bench.git",
        s.base().trim_start_matches("http://")
    );

    let started = Instant::now();
    let deadline = started + Duration::from_secs(seconds);
    let handles: Vec<_> = (0..clients)
        .map(|n| {
            let url = url.clone();
            std::thread::spawn(move || client(&url, n, deadline))
        })
        .collect();
    let mut times: Vec<f64> = Vec::new();
    for h in handles {
        match h.join().unwrap() {
            Ok(t) => times.extend(t),
            // The server's log says what Git's message doesn't.
            Err(why) => panic!("{why}\nserver log:\n{}", s.log()),
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    times.sort_by(f64::total_cmp);
    assert!(!times.is_empty(), "no push finished");
    let pushes = times.len();
    let per_second = pushes as f64 / elapsed;

    let git = GitClient::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let report = json!({
        "schema": "slath-push-throughput/1",
        "slath": {
            "version": env!("CARGO_PKG_VERSION"),
            "commit": git_output(&git, &root, &["rev-parse", "HEAD"]),
            "binary": slath_bin().display().to_string(),
        },
        "git": git_output(&git, &root, &["version"]),
        "git_binary": git_bin().display().to_string(),
        "hardware": hardware(),
        "clients": clients,
        "seconds": elapsed,
        "pushes": pushes,
        "pushes_per_second": per_second,
        "latency_ms": {
            "p50": quantile(&times, 0.50),
            "p95": quantile(&times, 0.95),
            "p99": quantile(&times, 0.99),
        },
    });
    let hw = &report["hardware"];
    let md = format!(
        "## Push throughput\n\n\
         **{per_second:.2} pushes per second** ({pushes} pushes by {clients} clients in {elapsed:.1} s)\n\n\
         | Push latency | ms |\n| --- | ---: |\n| p50 | {:.0} |\n| p95 | {:.0} |\n| p99 | {:.0} |\n\n\
         | Ran on | |\n| --- | --- |\n| CPU | {} |\n| Logical CPUs | {} |\n| Memory | {} MiB |\n\
         | Kernel | {} |\n| Git | {} |\n| Slath | {} ({}) |\n",
        quantile(&times, 0.50),
        quantile(&times, 0.95),
        quantile(&times, 0.99),
        hw["cpu"].as_str().unwrap_or("unknown"),
        hw["logical_cpus"],
        hw["memory_mib"],
        hw["kernel"].as_str().unwrap_or("unknown"),
        report["git"].as_str().unwrap_or("unknown"),
        env!("CARGO_PKG_VERSION"),
        report["slath"]["commit"]
            .as_str()
            .unwrap_or("unknown commit"),
    );
    let dir = report_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("push-throughput.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("push-throughput.md"), &md).unwrap();
    eprintln!("{md}");

    // Every push landed: each client's branch is where its last push left it.
    let refs = git_output(
        &git,
        &s.repo_path(&slath_tests::server::TestRepo {
            namespace: "alice",
            name: "bench",
        }),
        &["for-each-ref", "--format=%(refname)", "refs/heads/bench/"],
    )
    .unwrap_or_default();
    assert_eq!(refs.lines().count() as u64, clients, "{refs}");
}
