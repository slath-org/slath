// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Loading the configuration: a TOML file overridden by
//! `SLATH__<SECTION>__<KEY>` environment variables, with secrets read from
//! `*_file` keys. Unknown keys are errors that name the key.

use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Env, Format, Toml};
use serde::Deserialize;
use slath_core::Secret;
use slath_core::config::Config;
use slath_core::ids::RepoId;
use slath_core::names::{NamespaceName, RepoName};

/// Default location of the configuration file.
pub const DEFAULT_PATH: &str = "/etc/slath/slath.toml";

/// A loaded and validated configuration.
#[derive(Debug)]
pub struct Loaded {
    pub cfg: Config,
    /// Legacy transport-test section; removed once repositories, tokens, and
    /// keys live in the database.
    pub spike: Option<Spike>,
}

impl std::ops::Deref for Loaded {
    type Target = Config;
    fn deref(&self) -> &Config {
        &self.cfg
    }
}

/// Loads `path` plus environment overrides from the process environment.
pub fn load(path: &Path) -> Result<Loaded, String> {
    load_with_env(path, Env::prefixed("SLATH__").split("__"))
}

fn load_with_env(path: &Path, env: Env) -> Result<Loaded, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut table: toml::Table =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let spike = table
        .remove("spike")
        .map(|v| v.try_into::<Spike>())
        .transpose()
        .map_err(|e| format!("{}: [spike]: {e}", path.display()))?;
    let cleaned = toml::to_string(&table).map_err(|e| e.to_string())?;
    let cfg: Config = Figment::new()
        .merge(Toml::string(&cleaned))
        .merge(env)
        .extract()
        .map_err(|e| format!("{}: {}", path.display(), describe(&e)))?;
    validate(&cfg)?;
    Ok(Loaded { cfg, spike })
}

/// Formats a figment error with the full key path (`server.listen: …`).
fn describe(e: &figment::Error) -> String {
    let mut out = Vec::new();
    for err in e.clone() {
        let key = err.path.join(".");
        if key.is_empty() {
            out.push(err.kind.to_string());
        } else {
            out.push(format!("{key}: {}", err.kind));
        }
    }
    out.join("; ")
}

fn validate(cfg: &Config) -> Result<(), String> {
    for (what, p) in [
        ("storage.data_dir", &cfg.storage.data_dir),
        ("storage.run_dir", &cfg.storage.run_dir),
        ("git.binary", &cfg.git.binary),
    ] {
        if !p.is_absolute() {
            return Err(format!("{what} must be an absolute path"));
        }
    }
    if cfg.database.url.is_some() && cfg.database.url_file.is_some() {
        return Err("database: set either url or url_file, not both".to_owned());
    }
    if cfg.auth.session_idle_days == 0
        || cfg.auth.session_absolute_days < cfg.auth.session_idle_days
    {
        return Err("auth: session_idle_days must be > 0 and <= session_absolute_days".to_owned());
    }
    if cfg.limits.api_page_max == 0 || cfg.limits.job_workers == 0 {
        return Err("limits: api_page_max and job_workers must be > 0".to_owned());
    }
    Ok(())
}

/// Reads a secret from a file: the contents with surrounding whitespace removed.
pub fn read_secret_file(path: &Path) -> Result<Secret<String>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let value = text.trim();
    if value.is_empty() {
        return Err(format!("{} is empty", path.display()));
    }
    Ok(Secret::new(value.to_owned()))
}

/// The database URL from `database.url` or `database.url_file`.
pub fn database_url(cfg: &Config) -> Result<Secret<String>, String> {
    match (&cfg.database.url, &cfg.database.url_file) {
        (Some(url), None) => Ok(Secret::new(url.expose().clone())),
        (None, Some(file)) => read_secret_file(file),
        _ => Err("database: url or url_file is required".to_owned()),
    }
}

/// Legacy transport-test section: config-defined repositories and one static token.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spike {
    /// Password accepted for HTTP Basic auth (any non-empty username).
    #[serde(deserialize_with = "spike_token")]
    pub token: Secret<String>,
    #[serde(default)]
    pub repos: Vec<SpikeRepo>,
    #[serde(default)]
    pub ssh_keys: Vec<SpikeSshKey>,
}

/// An SSH key and the spike repositories (`namespace/name`) it may use.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpikeSshKey {
    pub id: String,
    /// One `authorized_keys`-style line: `<type> <base64> [comment]`.
    pub public_key: String,
    #[serde(default)]
    pub repos: Vec<String>,
}

impl SpikeSshKey {
    /// Splits the key into type and base64 blob and computes the OpenSSH
    /// fingerprint (`SHA256:` + unpadded base64 of the blob's SHA-256).
    pub fn parse(&self) -> Result<(String, String, String), String> {
        use base64::Engine;
        use sha2::Digest;
        let mut parts = self.public_key.split_whitespace();
        let (Some(kind), Some(b64)) = (parts.next(), parts.next()) else {
            return Err(format!("ssh key {}: expected '<type> <base64>'", self.id));
        };
        let blob = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("ssh key {}: {e}", self.id))?;
        let digest = sha2::Sha256::digest(&blob);
        let fp = base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest);
        Ok((kind.to_owned(), b64.to_owned(), format!("SHA256:{fp}")))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpikeRepo {
    pub id: RepoId,
    pub namespace: NamespaceName,
    pub name: RepoName,
    #[serde(default = "default_branch")]
    pub default_branch: String,
}

fn default_branch() -> String {
    "main".to_owned()
}

fn spike_token<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Secret<String>, D::Error> {
    let s = String::deserialize(d)?;
    if s.len() < 16 {
        return Err(serde::de::Error::custom(
            "token must be at least 16 characters",
        ));
    }
    Ok(Secret::new(s))
}

/// Resolves the config path: `--config`, else `$SLATH_CONFIG`, else the default.
pub fn resolve_path(flag: Option<PathBuf>) -> PathBuf {
    flag.or_else(|| std::env::var_os("SLATH_CONFIG").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PATH))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        [server]
        public_url = "https://git.example.org"
        [storage]
        data_dir = "/d"
        run_dir = "/r"
    "#;

    #[allow(clippy::result_large_err)] // figment::Jail's closure signature
    fn load_str(text: &str, env: &[(&str, &str)]) -> Result<Loaded, String> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slath.toml");
        std::fs::write(&path, text).unwrap();
        // A unique prefix keeps tests independent of the real environment.
        let prefix = format!("SLATHTEST{}__", std::process::id());
        let vars: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| (format!("{prefix}{k}"), (*v).to_owned()))
            .collect();
        let mut result = None;
        figment::Jail::expect_with(|jail| {
            for (k, v) in &vars {
                jail.set_env(k, v);
            }
            result = Some(load_with_env(&path, Env::prefixed(&prefix).split("__")));
            Ok(())
        });
        result.unwrap()
    }

    #[test]
    fn minimal_config_gets_defaults() {
        let cfg = load_str(MINIMAL, &[]).unwrap();
        assert_eq!(cfg.server.public_url.origin(), "https://git.example.org");
        assert_eq!(cfg.limits.diff_max_files, 300);
        assert_eq!(cfg.limits.push_max_bytes.get(), 2 << 30);
        assert_eq!(cfg.git.ssh_port, 22);
        assert!(cfg.spike.is_none());
    }

    #[test]
    fn unknown_keys_are_errors_naming_the_key() {
        let err = load_str(
            &MINIMAL.replace("[storage]", "[storage]\ndata_dri = \"/x\""),
            &[],
        )
        .unwrap_err();
        assert!(err.contains("data_dri"), "{err}");
        let err = load_str(&format!("{MINIMAL}\n[limits]\nbogus_limit = 3\n"), &[]).unwrap_err();
        assert!(err.contains("bogus_limit"), "{err}");
        let err = load_str(MINIMAL, &[("LIMITS__NOPE", "1")]).unwrap_err();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn missing_public_url_is_an_error() {
        let err = load_str(
            "[server]\n[storage]\ndata_dir = \"/d\"\nrun_dir = \"/r\"\n",
            &[],
        )
        .unwrap_err();
        assert!(err.contains("public_url"), "{err}");
    }

    #[test]
    fn environment_overrides_the_file() {
        let cfg = load_str(
            MINIMAL,
            &[
                ("LIMITS__DIFF_MAX_FILES", "12"),
                ("LIMITS__GIT_READ_TIMEOUT", "5s"),
                ("SERVER__PUBLIC_URL", "http://localhost:3000"),
            ],
        )
        .unwrap();
        assert_eq!(cfg.limits.diff_max_files, 12);
        assert_eq!(cfg.limits.git_read_timeout.get().as_secs(), 5);
        assert_eq!(cfg.server.public_url.origin(), "http://localhost:3000");
    }

    #[test]
    fn relative_paths_and_bad_values_are_rejected() {
        assert!(load_str(&MINIMAL.replace("\"/d\"", "\"d\""), &[]).is_err());
        let err = load_str(
            &format!("{MINIMAL}\n[limits]\ngit_read_timeout = \"1y\"\n"),
            &[],
        )
        .unwrap_err();
        assert!(err.contains("git_read_timeout"), "{err}");
    }

    #[test]
    fn database_url_from_file_and_secrets_stay_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("db-url");
        std::fs::write(&f, "postgres://u:hunter2@h/db\n").unwrap();
        let text = format!("{MINIMAL}\n[database]\nurl_file = \"{}\"\n", f.display());
        let cfg = load_str(&text, &[]).unwrap();
        let url = database_url(&cfg).unwrap();
        assert_eq!(url.expose(), "postgres://u:hunter2@h/db");
        let inline = format!("{MINIMAL}\n[database]\nurl = \"postgres://u:hunter2@h/db\"\n");
        let cfg = load_str(&inline, &[]).unwrap();
        assert!(!format!("{:?}", cfg.cfg).contains("hunter2"));
        let both = format!(
            "{MINIMAL}\n[database]\nurl = \"x\"\nurl_file = \"{}\"\n",
            f.display()
        );
        assert!(load_str(&both, &[]).is_err());
    }

    #[test]
    fn spike_section_is_split_off() {
        let text = format!(
            "{MINIMAL}\n[spike]\ntoken = \"0123456789abcdef\"\n[[spike.repos]]\n\
             id = \"0192f1a2-3b4c-7d5e-8f60-718293a4b5c6\"\nnamespace = \"alice\"\nname = \"demo\"\n"
        );
        let cfg = load_str(&text, &[]).unwrap();
        let spike = cfg.spike.unwrap();
        assert_eq!(spike.repos[0].default_branch, "main");
        assert!(!format!("{spike:?}").contains("0123456789abcdef"));
    }

    #[test]
    fn ssh_key_fingerprints_match_openssh() {
        // `ssh-keygen -lf` of this key prints the fingerprint below.
        let key = SpikeSshKey {
            id: "k".into(),
            public_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl test".into(),
            repos: vec![],
        };
        let (kind, _, fp) = key.parse().unwrap();
        assert_eq!(kind, "ssh-ed25519");
        assert_eq!(fp, "SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU");
    }
}
