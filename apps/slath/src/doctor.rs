// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath doctor`: deterministic diagnostics of an installation. Each
//! problem has a stable code and a remediation; `--output json` is for
//! monitoring (schema: `docs/cli/doctor.schema.json`). It changes nothing,
//! needs no extra privileges, contacts nothing but the instance's own
//! public URL, and prints no secrets.

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use serde::Serialize;
use serde_json::json;

use crate::config::{self, Loaded};
use crate::output::{OutputFormat, print_json, runtime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
pub struct Finding {
    /// Stable, documented code, e.g. `ssh.boundary`.
    pub code: &'static str,
    pub status: Status,
    pub detail: String,
    /// What to do about it (empty when all is well).
    pub fix: String,
}

impl Finding {
    fn ok(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            status: Status::Ok,
            detail: detail.into(),
            fix: String::new(),
        }
    }
    fn warn(code: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            code,
            status: Status::Warn,
            detail: detail.into(),
            fix: fix.into(),
        }
    }
    fn fail(code: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            code,
            status: Status::Fail,
            detail: detail.into(),
            fix: fix.into(),
        }
    }
}

/// Free and total bytes of the file system holding `path`.
fn disk(path: &Path) -> Option<(u64, u64)> {
    let existing = path.ancestors().find(|p| p.exists())?;
    let st = nix::sys::statvfs::statvfs(existing).ok()?;
    let f = st.fragment_size() as u64;
    Some((st.blocks_available() as u64 * f, st.blocks() as u64 * f))
}

/// Classifies free disk space.
fn disk_finding(free: u64, total: u64) -> Finding {
    const GIB: u64 = 1 << 30;
    let pct = (free * 100).checked_div(total).unwrap_or(100);
    let detail = format!("{:.1} GiB free ({pct}%)", free as f64 / GIB as f64);
    let fix = "free space on the data directory's file system, or move it to a larger one";
    if free < GIB || pct < 5 {
        Finding::fail("disk.low", detail, fix)
    } else if pct < 15 {
        Finding::warn("disk.low", detail, fix)
    } else {
        Finding::ok("disk.low", detail)
    }
}

/// The newest backup under `dir` (a directory with a `manifest.json`)
/// and its age.
fn newest_backup(dir: &Path) -> Option<(String, Duration)> {
    let now = SystemTime::now();
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let m = e.path().join("manifest.json").metadata().ok()?;
            let age = now.duration_since(m.modified().ok()?).unwrap_or_default();
            Some((e.file_name().to_string_lossy().into_owned(), age))
        })
        .min_by_key(|(_, age)| *age)
}

/// Classifies backup freshness: `max_age` is how old the newest backup
/// may be (from the schedule; a week without one).
fn backup_finding(
    newest: Option<(String, Duration)>,
    max_age: Duration,
    scheduled: bool,
) -> Finding {
    let fix = if scheduled {
        "check the server log and the admin Upgrades page for failed scheduled backups"
    } else {
        "set [backup] schedule, or run `slath backup create` regularly and copy backups off the host"
    };
    match newest {
        None => Finding::warn("backup.none", "no backup in the backup directory", fix),
        Some((name, age)) if age > max_age => Finding::warn(
            "backup.stale",
            format!(
                "the newest backup, {name}, is {} hours old",
                age.as_secs() / 3600
            ),
            fix,
        ),
        Some((name, age)) => Finding::ok(
            "backup.stale",
            format!("newest backup {name}, {} hours old", age.as_secs() / 3600),
        ),
    }
}

/// Classifies a certificate's remaining validity.
fn tls_expiry_finding(host: &str, days_left: i64, trusted: Result<(), String>) -> Finding {
    let renew =
        "renew the certificate (Caddy renews automatically if it can reach the ACME server)";
    if days_left < 0 {
        return Finding::fail(
            "tls.expiry",
            format!("the certificate of {host} expired {} days ago", -days_left),
            renew,
        );
    }
    if let Err(why) = trusted {
        return Finding::warn(
            "tls.trust",
            format!(
                "the certificate of {host} isn't trusted by the public roots ({why}); fine for a private CA if clients trust it"
            ),
            "use a certificate from a public CA, or distribute your CA to every client",
        );
    }
    if days_left < 14 {
        return Finding::warn(
            "tls.expiry",
            format!("the certificate of {host} expires in {days_left} days"),
            renew,
        );
    }
    Finding::ok(
        "tls.expiry",
        format!("the certificate of {host} is valid for {days_left} more days"),
    )
}

/// Connects to the instance's own HTTPS address and reads its certificate.
fn check_tls(public_url: &str) -> Finding {
    let Ok(url) = url::Url::parse(public_url) else {
        return Finding::fail(
            "tls.expiry",
            "server.public_url isn't a URL",
            "fix server.public_url",
        );
    };
    if url.scheme() != "https" {
        return Finding::ok(
            "tls.expiry",
            "public_url uses http; TLS is terminated elsewhere or not used",
        );
    }
    let host = url.host_str().unwrap_or_default().to_owned();
    let port = url.port_or_known_default().unwrap_or(443);
    match tls::peer_certificate(&host, port) {
        Ok((der, trusted)) => match tls::days_left(&der) {
            Some(days) => tls_expiry_finding(&host, days, trusted),
            None => Finding::warn(
                "tls.expiry",
                format!("can't read the certificate of {host}"),
                "check the TLS configuration",
            ),
        },
        Err(e) => Finding::warn(
            "tls.unreachable",
            format!("can't connect to {host}:{port}: {e}"),
            "check that public_url is reachable from this host",
        ),
    }
}

mod tls {
    use std::io::Write;
    use std::net::{TcpStream, ToSocketAddrs};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};

    /// A certificate (DER) and whether the public roots trust it.
    pub type Seen = (Vec<u8>, Result<(), String>);

    /// Accepts any certificate, so its expiry can be read even when it
    /// isn't trusted, and records whether the public roots trust it.
    #[derive(Debug)]
    struct Recorder {
        inner: Arc<rustls::client::WebPkiServerVerifier>,
        seen: Mutex<Option<Seen>>,
    }

    impl ServerCertVerifier for Recorder {
        fn verify_server_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            intermediates: &[CertificateDer<'_>],
            server_name: &ServerName<'_>,
            ocsp: &[u8],
            now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            let trusted = self
                .inner
                .verify_server_cert(end_entity, intermediates, server_name, ocsp, now)
                .map(|_| ())
                .map_err(|e| e.to_string());
            if let Ok(mut s) = self.seen.lock() {
                *s = Some((end_entity.to_vec(), trusted));
            }
            Ok(ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            m: &[u8],
            c: &CertificateDer<'_>,
            d: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            self.inner.verify_tls12_signature(m, c, d)
        }
        fn verify_tls13_signature(
            &self,
            m: &[u8],
            c: &CertificateDer<'_>,
            d: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            self.inner.verify_tls13_signature(m, c, d)
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.inner.supported_verify_schemes()
        }
    }

    /// The server's end-entity certificate (DER) and whether the public
    /// roots trust it.
    pub fn peer_certificate(host: &str, port: u16) -> Result<Seen, String> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let inner = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            Arc::clone(&provider),
        )
        .build()
        .map_err(|e| e.to_string())?;
        let recorder = Arc::new(Recorder {
            inner,
            seen: Mutex::new(None),
        });
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .dangerous()
            .with_custom_certificate_verifier(Arc::clone(&recorder) as Arc<dyn ServerCertVerifier>)
            .with_no_client_auth();
        let name = ServerName::try_from(host.to_owned()).map_err(|e| e.to_string())?;
        let mut conn =
            rustls::ClientConnection::new(Arc::new(config), name).map_err(|e| e.to_string())?;
        let addr = (host, port)
            .to_socket_addrs()
            .map_err(|e| e.to_string())?
            .next()
            .ok_or("no address")?;
        let mut sock =
            TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| e.to_string())?;
        sock.set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        sock.set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        while conn.is_handshaking() {
            conn.complete_io(&mut sock).map_err(|e| e.to_string())?;
        }
        conn.send_close_notify();
        let _ = conn.complete_io(&mut sock);
        let _ = sock.flush();
        recorder
            .seen
            .lock()
            .ok()
            .and_then(|mut s| s.take())
            .ok_or_else(|| "the server sent no certificate".to_owned())
    }

    /// Whole days until the certificate's `notAfter` (negative if past).
    pub fn days_left(der: &[u8]) -> Option<i64> {
        let (_, cert) = x509_parser::parse_x509_certificate(der).ok()?;
        let not_after = cert.validity().not_after.timestamp();
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        Some((not_after - now).div_euclid(86_400))
    }
}

/// Runs every check.
fn run_checks(path: &Path) -> Vec<Finding> {
    let cfg = match config::load(path) {
        Ok(c) => c,
        Err(e) => {
            return vec![Finding::fail(
                "config.invalid",
                e,
                "fix the configuration; `slath config check` shows details",
            )];
        }
    };
    let mut out = vec![Finding::ok(
        "config.invalid",
        format!("{} is valid", path.display()),
    )];
    for w in &cfg.warnings {
        out.push(Finding::warn(
            "config.deprecated",
            w.clone(),
            "run `slath config migrate` on the file",
        ));
    }
    match crate::check::run_checks(&cfg) {
        Ok(items) => {
            for (name, ok, detail) in items {
                let (code, fix) = match name {
                    "git" => (
                        "git.version",
                        "install the newest stable Git and set git.binary",
                    ),
                    "data_dir" => (
                        "storage.writable",
                        "give the service account write access to storage.data_dir",
                    ),
                    "socket" => (
                        "ssh.boundary",
                        "remove group and world access from the listed paths (chmod o-rwx, chmod g-rwx)",
                    ),
                    "database" => (
                        "db.unreachable",
                        "check database.url or database.url_file and that PostgreSQL runs",
                    ),
                    "master_key" | "old_key" => (
                        "keys.unreadable",
                        "restore the master key file; without it, secrets can't be decrypted",
                    ),
                    _ => ("check.other", "see `slath config check`"),
                };
                out.push(if ok {
                    Finding::ok(code, detail)
                } else {
                    Finding::fail(code, detail, fix)
                });
            }
        }
        Err(e) => out.push(Finding::fail("check.other", e, "see `slath config check`")),
    }
    if let Some((free, total)) = disk(&cfg.storage.data_dir) {
        out.push(disk_finding(free, total));
    }
    let backups = cfg.storage.data_dir.join("backups");
    let scheduled = cfg.backup.schedule.is_some();
    let max_age = Duration::from_secs(if scheduled { 2 } else { 7 } * 86_400);
    out.push(backup_finding(newest_backup(&backups), max_age, scheduled));
    out.extend(schema_and_update(&cfg));
    out.push(check_tls(&cfg.server.public_url.join("/")));
    out
}

/// Schema state and the update preflight, if the database is reachable.
fn schema_and_update(cfg: &Loaded) -> Vec<Finding> {
    let result = (|| {
        let rt = runtime()?;
        rt.block_on(async {
            let mut pc = slath_db::PoolConfig::new(config::database_url(cfg)?);
            pc.max_connections = 2;
            let pool = slath_db::connect(&pc).await.map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            match slath_db::check_schema(&pool).await {
                Ok(()) => out.push(Finding::ok(
                    "schema.state",
                    format!("schema {} is current", slath_db::REQUIRED_SCHEMA),
                )),
                Err(slath_db::DbError::SchemaBehind { found, required }) => {
                    out.push(Finding::warn(
                        "schema.state",
                        format!("schema {found}, this version needs {required}"),
                        "restart the server to update (it backs up first), or run `slath upgrade`",
                    ))
                }
                Err(e) => out.push(Finding::fail(
                    "schema.state",
                    e.to_string(),
                    "see docs/admin/upgrading.md",
                )),
            }
            if let Ok(Some(v)) = slath_db::settings::get(&pool, "scheduled_backup").await
                && let Ok(last) = serde_json::from_value::<slath_service::admin::ScheduledBackup>(v)
            {
                out.push(match last.result {
                    Ok(path) => Finding::ok("backup.failed", format!("the last scheduled backup succeeded: {path}")),
                    Err(e) => Finding::fail(
                        "backup.failed",
                        format!("the last scheduled backup failed: {e}"),
                        "fix the cause (often disk space or permissions on <data_dir>/backups); the next run retries",
                    ),
                });
            }
            for (item, failed, detail) in crate::upgrade::preflight_problems(cfg, &pool).await? {
                let fix = "see `slath upgrade --check`";
                let code = match item {
                    "postgres" => "update.postgres",
                    "role" => "postgres.superuser",
                    "extensions" => "update.extensions",
                    "git" => "update.git",
                    "source" => "update.source",
                    "action" => "update.action",
                    "config" => "update.config",
                    "data_dir" => "update.data_dir",
                    "lock" => "update.lock",
                    _ => "update.other",
                };
                out.push(if failed {
                    Finding::fail(code, detail, fix)
                } else {
                    Finding::warn(code, detail, fix)
                });
            }
            Ok::<_, String>(out)
        })
    })();
    // An unreachable database is already reported by `db.unreachable`.
    result.unwrap_or_default()
}

/// `slath doctor [--output json]`. Exit status 1 if any check failed.
pub fn run(path: &Path, format: OutputFormat) -> ExitCode {
    let checks = run_checks(path);
    let failed = checks.iter().any(|c| c.status == Status::Fail);
    match format {
        OutputFormat::Text => {
            for c in &checks {
                let mark = match c.status {
                    Status::Ok => "ok  ",
                    Status::Warn => "WARN",
                    Status::Fail => "FAIL",
                };
                println!("{mark} {:<18} {}", c.code, c.detail);
                if !c.fix.is_empty() {
                    println!("     {:<18} fix: {}", "", c.fix);
                }
            }
            let warn = checks.iter().filter(|c| c.status == Status::Warn).count();
            let fail = checks.iter().filter(|c| c.status == Status::Fail).count();
            println!("{fail} problem(s), {warn} warning(s)");
        }
        OutputFormat::Json => print_json(&json!({"ok": !failed, "checks": checks})),
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_thresholds() {
        const GIB: u64 = 1 << 30;
        assert_eq!(disk_finding(50 * GIB, 100 * GIB).status, Status::Ok);
        assert_eq!(disk_finding(10 * GIB, 100 * GIB).status, Status::Warn);
        assert_eq!(disk_finding(GIB / 2, 100 * GIB).status, Status::Fail);
        assert_eq!(disk_finding(4 * GIB, 100 * GIB).status, Status::Fail);
    }

    #[test]
    fn backup_freshness() {
        let day = Duration::from_secs(86_400);
        assert_eq!(backup_finding(None, 7 * day, false).code, "backup.none");
        let old = backup_finding(Some(("b".into(), 9 * day)), 7 * day, false);
        assert_eq!((old.code, old.status), ("backup.stale", Status::Warn));
        assert_eq!(
            backup_finding(Some(("b".into(), day)), 7 * day, false).status,
            Status::Ok
        );
    }

    #[test]
    fn tls_expiry() {
        assert_eq!(tls_expiry_finding("h", -1, Ok(())).status, Status::Fail);
        assert_eq!(tls_expiry_finding("h", 5, Ok(())).status, Status::Warn);
        assert_eq!(tls_expiry_finding("h", 60, Ok(())).status, Status::Ok);
        let private = tls_expiry_finding("h", 60, Err("unknown issuer".into()));
        assert_eq!((private.code, private.status), ("tls.trust", Status::Warn));
    }

    #[test]
    fn certificate_expiry_is_read() {
        // A self-signed certificate valid until 2049-12-31 (test data).
        let pem = include_str!("../tests/data/doctor-cert.pem");
        let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        let der = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64).unwrap();
        let days = tls::days_left(&der).unwrap();
        assert!(days > 365 * 20, "{days}");
    }
}
