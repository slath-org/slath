// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed configuration with defaults.
//!
//! These are plain data types: loading (TOML file, `SLATH__SECTION__KEY`
//! environment overrides, `*_file` secrets) lives in the `slath` binary.
//! Every struct rejects unknown keys, so a typo is an error that names the key.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer};

use crate::Secret;

/// The complete configuration.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: Server,
    #[serde(default)]
    pub database: Database,
    pub storage: Storage,
    #[serde(default)]
    pub secrets: Secrets,
    #[serde(default)]
    pub git: Git,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub smtp: Smtp,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub ci: Ci,
    #[serde(default)]
    pub maintenance: Maintenance,
    #[serde(default)]
    pub ui: Ui,
    #[serde(default)]
    pub import: Import,
    #[serde(default)]
    pub upgrade: Upgrade,
    #[serde(default)]
    pub log: Log,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    /// Origin used for links, cookies, and CSRF checks. Required.
    pub public_url: PublicUrl,
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// `X-Forwarded-For` is honoured only from these networks.
    #[serde(default = "default_trusted_proxies")]
    pub trusted_proxies: Vec<String>,
    /// Optional separate origin for raw files and attachments.
    #[serde(default)]
    pub usercontent_url: String,
    /// Prometheus listener; empty disables it.
    #[serde(default = "default_metrics_listen")]
    pub metrics_listen: String,
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 3000))
}

fn default_trusted_proxies() -> Vec<String> {
    vec!["127.0.0.1/32".to_owned()]
}

fn default_metrics_listen() -> String {
    "127.0.0.1:9100".to_owned()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Database {
    /// Connection URL. Prefer `url_file`.
    #[serde(default)]
    pub url: Option<Secret<String>>,
    /// File containing the connection URL.
    #[serde(default)]
    pub url_file: Option<PathBuf>,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

fn default_max_connections() -> u32 {
    20
}

impl Default for Database {
    fn default() -> Self {
        Self {
            url: None,
            url_file: None,
            max_connections: default_max_connections(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Storage {
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_run_dir")]
    pub run_dir: PathBuf,
    /// Group (name or numeric ID) whose members may connect to the IPC
    /// socket: the SSH login's group. Unset, only the server's user can.
    #[serde(default, deserialize_with = "name_or_id")]
    pub socket_group: Option<String>,
}

/// A group given by name or numeric ID (`"git"`, `"10002"`, or `10002`, as
/// environment overrides of digits arrive as numbers).
fn name_or_id<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Group {
        Name(String),
        Id(u32),
    }
    Ok(Option::<Group>::deserialize(d)?.map(|g| match g {
        Group::Name(n) => n,
        Group::Id(id) => id.to_string(),
    }))
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("/var/lib/slath")
}

fn default_run_dir() -> PathBuf {
    PathBuf::from("/run/slath")
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Secrets {
    #[serde(default)]
    pub master_key_file: Option<PathBuf>,
    #[serde(default)]
    pub old_key_files: Vec<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Git {
    #[serde(default = "default_git_binary")]
    pub binary: PathBuf,
    #[serde(default = "default_ssh_user")]
    pub ssh_user: String,
    /// Host shown in SSH clone URLs; empty means the host of `public_url`.
    #[serde(default)]
    pub ssh_host: String,
    #[serde(default = "default_ssh_port")]
    pub ssh_port: u16,
}

fn default_git_binary() -> PathBuf {
    PathBuf::from("/usr/bin/git")
}

fn default_ssh_user() -> String {
    "git".to_owned()
}

const fn default_ssh_port() -> u16 {
    22
}

impl Default for Git {
    fn default() -> Self {
        Self {
            binary: default_git_binary(),
            ssh_user: default_ssh_user(),
            ssh_host: String::new(),
            ssh_port: default_ssh_port(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Registration {
    #[default]
    Disabled,
    Invite,
    Open,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    #[serde(default)]
    pub registration: Registration,
    #[serde(default = "yes")]
    pub require_mfa_for_admins: bool,
    #[serde(default = "default_idle_days")]
    pub session_idle_days: u32,
    #[serde(default = "default_absolute_days")]
    pub session_absolute_days: u32,
    /// Whether any user may create organizations (otherwise site admins only).
    #[serde(default = "yes")]
    pub users_can_create_orgs: bool,
    /// OpenID Connect providers people can sign in with.
    #[serde(default)]
    pub oidc: Vec<OidcProvider>,
}

/// An OpenID Connect provider (`[[auth.oidc]]`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcProvider {
    /// Short identifier used in URLs, e.g. `company`.
    pub key: String,
    /// Shown on the sign-in button.
    pub display_name: String,
    /// Must match the `iss` of the provider's tokens exactly.
    pub issuer: String,
    pub client_id: String,
    /// File holding the client secret.
    pub client_secret_file: PathBuf,
    #[serde(default = "default_oidc_scopes")]
    pub scopes: Vec<String>,
    /// Use the provider's verified email to offer linking to an existing
    /// account (after the person signs in to it).
    #[serde(default)]
    pub trust_email: bool,
    /// Create an account at the first sign-in with an unknown identity.
    #[serde(default)]
    pub allow_registration: bool,
    /// Sign-ins through this provider satisfy second-factor requirements.
    #[serde(default)]
    pub counts_as_mfa: bool,
}

fn default_oidc_scopes() -> Vec<String> {
    vec![
        "openid".to_owned(),
        "email".to_owned(),
        "profile".to_owned(),
    ]
}

const fn yes() -> bool {
    true
}

const fn default_idle_days() -> u32 {
    14
}

const fn default_absolute_days() -> u32 {
    30
}

impl Default for Auth {
    fn default() -> Self {
        Self {
            registration: Registration::default(),
            require_mfa_for_admins: true,
            session_idle_days: default_idle_days(),
            session_absolute_days: default_absolute_days(),
            users_can_create_orgs: true,
            oidc: Vec::new(),
        }
    }
}

/// How the SMTP connection is secured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SmtpTls {
    /// Plain connection upgraded with STARTTLS (required, not opportunistic).
    #[default]
    Starttls,
    /// TLS from the start (port 465).
    Tls,
    /// No encryption: only for a relay on the same host or a test catcher.
    None,
}

/// Outbound email. Without `host`, email is off and the UI hides it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Smtp {
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    #[serde(default)]
    pub tls: SmtpTls,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password_file: Option<PathBuf>,
    /// Sender, e.g. `Slath <noreply@example.org>`.
    #[serde(default)]
    pub from: String,
}

impl Smtp {
    /// True if email is configured.
    #[must_use]
    pub fn enabled(&self) -> bool {
        !self.host.trim().is_empty()
    }
}

const fn default_smtp_port() -> u16 {
    587
}

impl Default for Smtp {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: default_smtp_port(),
            tls: SmtpTls::default(),
            username: String::new(),
            password_file: None,
            from: String::new(),
        }
    }
}

/// Every limit Slath enforces. Nothing is unbounded.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    /// Tree, blob, log, diff, and blame operations.
    pub git_read_timeout: Dur,
    /// Upload/receive-pack with no I/O.
    pub git_transport_idle_timeout: Dur,
    /// Concurrent Git processes; 0 means 2 × CPUs.
    pub git_max_concurrent: usize,
    /// Maximum pack size of one push.
    pub push_max_bytes: Bytes,
    /// Size above which a warning is shown after a push; 0 disables it.
    pub repo_soft_quota_bytes: Bytes,
    /// Largest file that is highlighted or rendered as Markdown.
    pub render_max_file_bytes: Bytes,
    pub diff_max_files: usize,
    pub diff_max_lines: usize,
    pub review_max_commits: usize,
    /// Request bodies (API and UI); pushes are limited separately.
    pub http_max_body_bytes: Bytes,
    pub attachment_max_bytes: Bytes,
    /// The longest a repository import may run.
    pub import_timeout: Dur,
    /// Uploaded avatar images (before re-encoding).
    pub avatar_max_bytes: Bytes,
    /// How long an unused attachment or avatar file is kept before it is
    /// deleted (backups taken meanwhile still find it).
    pub blob_grace_period: Dur,
    /// Issue and merge request descriptions and comments.
    pub text_max_bytes: Bytes,
    /// Maximum `limit` of list endpoints.
    pub api_page_max: usize,
    pub archive_timeout: Dur,
    /// Concurrent archive downloads.
    pub archive_max_concurrent: usize,
    /// API requests per hour for an authenticated principal.
    pub api_requests_per_hour: u32,
    /// API requests per hour per IP for anonymous clients.
    pub api_anonymous_requests_per_hour: u32,
    /// Failed logins per account per 15 minutes before throttling.
    pub login_failures_per_account: u32,
    /// Failed logins per IP per 15 minutes before throttling.
    pub login_failures_per_ip: u32,
    /// SSH key lookups per second (sshd's `AuthorizedKeysCommand`).
    pub ssh_key_lookups_per_second: u32,
    /// Background jobs run concurrently by one server.
    pub job_workers: usize,
    /// Items per batch of a background migration.
    pub background_migration_batch: u32,
    /// Pause between batches of a background migration (rate limit).
    pub background_migration_interval: Dur,
}

impl Default for Limits {
    fn default() -> Self {
        const MIB: u64 = 1024 * 1024;
        Self {
            git_read_timeout: Dur::secs(30),
            git_transport_idle_timeout: Dur::secs(600),
            git_max_concurrent: 0,
            push_max_bytes: Bytes(2 * 1024 * MIB),
            repo_soft_quota_bytes: Bytes(0),
            render_max_file_bytes: Bytes(MIB),
            diff_max_files: 300,
            diff_max_lines: 20_000,
            review_max_commits: 250,
            http_max_body_bytes: Bytes(10 * MIB),
            attachment_max_bytes: Bytes(25 * MIB),
            avatar_max_bytes: Bytes(5 * MIB),
            import_timeout: Dur::secs(3600),
            blob_grace_period: Dur::secs(86_400),
            text_max_bytes: Bytes(256 * 1024),
            api_page_max: 100,
            archive_timeout: Dur::secs(120),
            archive_max_concurrent: 4,
            api_requests_per_hour: 5000,
            api_anonymous_requests_per_hour: 300,
            login_failures_per_account: 10,
            login_failures_per_ip: 50,
            ssh_key_lookups_per_second: 200,
            job_workers: 4,
            background_migration_batch: 500,
            background_migration_interval: Dur::millis(100),
        }
    }
}

impl Limits {
    /// `git_max_concurrent`, with 0 resolved to 2 × CPUs.
    #[must_use]
    pub fn git_max_concurrent(&self, cpus: usize) -> usize {
        if self.git_max_concurrent == 0 {
            2 * cpus.max(1)
        } else {
            self.git_max_concurrent
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ci {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_ci_concurrent")]
    pub max_concurrent_per_repo: u32,
    #[serde(default = "default_ci_artifacts")]
    pub max_artifact_bytes_per_repo: Bytes,
    #[serde(default = "default_ci_retention")]
    pub run_retention_days: u32,
}

const fn default_ci_concurrent() -> u32 {
    4
}

const fn default_ci_artifacts() -> Bytes {
    Bytes(2 * 1024 * 1024 * 1024)
}

const fn default_ci_retention() -> u32 {
    90
}

impl Default for Ci {
    fn default() -> Self {
        Self {
            enabled: false,
            max_concurrent_per_repo: default_ci_concurrent(),
            max_artifact_bytes_per_repo: default_ci_artifacts(),
            run_retention_days: default_ci_retention(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Maintenance {
    #[serde(default = "default_reconcile_interval")]
    pub reconcile_interval: Dur,
    #[serde(default = "default_deleted_repo_days")]
    pub deleted_repo_days: u32,
    #[serde(default = "default_audit_retention_days")]
    pub audit_retention_days: u32,
}

const fn default_reconcile_interval() -> Dur {
    Dur::secs(3600)
}

const fn default_deleted_repo_days() -> u32 {
    7
}

const fn default_audit_retention_days() -> u32 {
    365
}

impl Default for Maintenance {
    fn default() -> Self {
        Self {
            reconcile_interval: default_reconcile_interval(),
            deleted_repo_days: default_deleted_repo_days(),
            audit_retention_days: default_audit_retention_days(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ui {
    /// Let user content (README images, badges) load from `https:` origins.
    #[serde(default = "yes")]
    pub external_images: bool,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            external_images: true,
        }
    }
}

/// Importing repositories from other servers.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Import {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Also allow `http://` URLs (credentials and data travel unencrypted).
    #[serde(default)]
    pub allow_http: bool,
    /// Networks that may be imported from although the address policy
    /// denies them (internal servers), e.g. `["10.20.0.0/16"]`.
    #[serde(default, deserialize_with = "cidrs")]
    pub allow_networks: Vec<crate::net::Cidr>,
}

fn cidrs<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<crate::net::Cidr>, D::Error> {
    Vec::<String>::deserialize(d)?
        .iter()
        .map(|s| s.parse().map_err(serde::de::Error::custom))
        .collect()
}

impl Default for Import {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_http: false,
            allow_networks: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upgrade {
    /// Back up before `serve` applies an update at start.
    #[serde(default = "yes")]
    pub auto_backup: bool,
    /// Newest pre-upgrade backups kept after a new one succeeds (0: all).
    #[serde(default = "default_backups_kept")]
    pub backups_kept: u32,
    /// Opt-in: ask `feed_url` for new releases once a day.
    #[serde(default)]
    pub check_for_updates: bool,
    #[serde(default = "default_feed_url")]
    pub feed_url: String,
}

impl Default for Upgrade {
    fn default() -> Self {
        Self {
            auto_backup: true,
            backups_kept: default_backups_kept(),
            check_for_updates: false,
            feed_url: default_feed_url(),
        }
    }
}

fn default_backups_kept() -> u32 {
    2
}

fn default_feed_url() -> String {
    "https://api.github.com/repos/slath-org/slath/releases".to_owned()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Log {
    #[serde(default)]
    pub format: LogFormat,
    #[serde(default = "default_level")]
    pub level: String,
}

fn default_level() -> String {
    "info".to_owned()
}

impl Default for Log {
    fn default() -> Self {
        Self {
            format: LogFormat::default(),
            level: default_level(),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Json,
    Pretty,
}

/// `server.public_url`: an `http`/`https` origin without path, query, or
/// fragment, stored without a trailing slash.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicUrl {
    origin: String,
    https: bool,
    host: String,
}

impl PublicUrl {
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim_end_matches('/');
        let (https, rest) = if let Some(r) = s.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = s.strip_prefix("http://") {
            (false, r)
        } else {
            return Err("public_url must start with http:// or https://".to_owned());
        };
        if rest.is_empty()
            || rest
                .bytes()
                .any(|b| matches!(b, b'/' | b'?' | b'#' | b'@') || b.is_ascii_whitespace())
        {
            return Err("public_url must be an origin like https://git.example.org".to_owned());
        }
        let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
            let (h, after) = v6
                .split_once(']')
                .ok_or("public_url has an unterminated IPv6 address")?;
            (h, after.strip_prefix(':'))
        } else {
            match rest.split_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (rest, None),
            }
        };
        if host.is_empty() || port.is_some_and(|p| p.parse::<u16>().is_err()) {
            return Err("public_url has an invalid host or port".to_owned());
        }
        Ok(Self {
            origin: format!("{}://{}", if https { "https" } else { "http" }, rest),
            https,
            host: host.to_owned(),
        })
    }

    /// `scheme://host[:port]`, without a trailing slash.
    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }

    #[must_use]
    pub const fn is_https(&self) -> bool {
        self.https
    }

    /// Host name without port or brackets.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Joins an absolute path (starting with `/`) to the origin.
    #[must_use]
    pub fn join(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }
}

impl fmt::Debug for PublicUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicUrl({})", self.origin)
    }
}

impl fmt::Display for PublicUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.origin)
    }
}

impl<'de> Deserialize<'de> for PublicUrl {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// A duration written as `500ms`, `30s`, `10m`, `1h`, `7d`, or plain seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Dur(pub Duration);

impl Dur {
    #[must_use]
    pub const fn secs(s: u64) -> Self {
        Self(Duration::from_secs(s))
    }

    #[must_use]
    pub const fn millis(ms: u64) -> Self {
        Self(Duration::from_millis(ms))
    }

    #[must_use]
    pub const fn get(self) -> Duration {
        self.0
    }
}

impl FromStr for Dur {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        let (num, unit) = s.split_at(split);
        let n: u64 = num
            .parse()
            .map_err(|_| format!("invalid duration {s:?} (examples: 30s, 10m, 1h)"))?;
        let d = match unit.trim() {
            "" | "s" => Duration::from_secs(n),
            "ms" => Duration::from_millis(n),
            "m" => Duration::from_secs(n.saturating_mul(60)),
            "h" => Duration::from_secs(n.saturating_mul(3600)),
            "d" => Duration::from_secs(n.saturating_mul(86_400)),
            _ => {
                return Err(format!(
                    "invalid duration unit in {s:?} (use ms, s, m, h, d)"
                ));
            }
        };
        Ok(Self(d))
    }
}

impl<'de> Deserialize<'de> for Dur {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Int(u64),
            Str(String),
        }
        match Raw::deserialize(d)? {
            Raw::Int(n) => Ok(Self::secs(n)),
            Raw::Str(s) => s.parse().map_err(serde::de::Error::custom),
        }
    }
}

/// A byte count written as an integer or with a unit (`10MiB`, `2GiB`, `512KiB`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Bytes(pub u64);

impl Bytes {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The value as `usize`, saturating on 32-bit targets.
    #[must_use]
    pub fn as_usize(self) -> usize {
        usize::try_from(self.0).unwrap_or(usize::MAX)
    }
}

impl FromStr for Bytes {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        let (num, unit) = s.split_at(split);
        let n: u64 = num
            .parse()
            .map_err(|_| format!("invalid size {s:?} (examples: 1048576, 10MiB, 2GiB)"))?;
        let mult: u64 = match unit.trim() {
            "" | "B" => 1,
            "KiB" => 1 << 10,
            "MiB" => 1 << 20,
            "GiB" => 1 << 30,
            "TiB" => 1 << 40,
            _ => {
                return Err(format!(
                    "invalid size unit in {s:?} (use B, KiB, MiB, GiB, TiB)"
                ));
            }
        };
        n.checked_mul(mult)
            .map(Self)
            .ok_or_else(|| format!("size {s:?} is too large"))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Int(u64),
            Str(String),
        }
        match Raw::deserialize(d)? {
            Raw::Int(n) => Ok(Self(n)),
            Raw::Str(s) => s.parse().map_err(serde::de::Error::custom),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!("30s".parse::<Dur>().unwrap().get(), Duration::from_secs(30));
        assert_eq!(
            "10m".parse::<Dur>().unwrap().get(),
            Duration::from_secs(600)
        );
        assert_eq!(
            "1h".parse::<Dur>().unwrap().get(),
            Duration::from_secs(3600)
        );
        assert_eq!(
            "250ms".parse::<Dur>().unwrap().get(),
            Duration::from_millis(250)
        );
        assert_eq!("45".parse::<Dur>().unwrap().get(), Duration::from_secs(45));
        assert!("1y".parse::<Dur>().is_err());
        assert!("h".parse::<Dur>().is_err());
    }

    #[test]
    fn byte_sizes() {
        assert_eq!("10MiB".parse::<Bytes>().unwrap().get(), 10 << 20);
        assert_eq!("2GiB".parse::<Bytes>().unwrap().get(), 2 << 30);
        assert_eq!("17".parse::<Bytes>().unwrap().get(), 17);
        assert!("10MB".parse::<Bytes>().is_err());
        assert!("99999999999TiB".parse::<Bytes>().is_err());
    }

    #[test]
    fn public_urls() {
        let u = PublicUrl::parse("https://git.example.org/").unwrap();
        assert_eq!(u.origin(), "https://git.example.org");
        assert_eq!(u.host(), "git.example.org");
        assert!(u.is_https());
        let u = PublicUrl::parse("http://127.0.0.1:3000").unwrap();
        assert_eq!(u.host(), "127.0.0.1");
        assert_eq!(u.join("/-/login"), "http://127.0.0.1:3000/-/login");
        let u = PublicUrl::parse("http://[::1]:8080").unwrap();
        assert_eq!(u.host(), "::1");
        for bad in [
            "git.example.org",
            "ftp://x",
            "https://",
            "https://x/path",
            "https://x?q",
            "https://u@x",
            "https://x:port",
        ] {
            assert!(PublicUrl::parse(bad).is_err(), "{bad}");
        }
    }
}

/// A configuration key that was renamed. Deprecated keys keep working for
/// at least two minor releases and are never removed within a major
/// version: loading maps them to their replacement with a warning, and
/// `slath config migrate` rewrites files to the replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deprecation {
    /// Dotted path of the old key, e.g. `ci.max_parallel`.
    pub old: &'static str,
    /// Dotted path of the replacement.
    pub new: &'static str,
    /// Release that deprecated the key.
    pub since: &'static str,
    /// Release that may remove it (a new major version).
    pub removal: &'static str,
}

/// Every deprecated key. Each entry also needs a `[[deprecation]]` entry in
/// the upgrade notes of the release that introduces it.
pub const DEPRECATED_KEYS: &[Deprecation] = &[];
