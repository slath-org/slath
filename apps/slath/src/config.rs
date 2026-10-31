// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Configuration file.
//!
//! A minimal subset: just enough for the transport spike. It will be replaced by
//! figment-based loading (environment overrides, `*_file` secrets, checks).
//! Unknown keys are errors.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use slath_core::Secret;
use slath_core::ids::RepoId;
use slath_core::names::{NamespaceName, RepoName};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: Server,
    pub storage: Storage,
    #[serde(default)]
    pub git: Git,
    #[serde(default)]
    pub log: Log,
    pub spike: Spike,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 3000))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Storage {
    pub data_dir: PathBuf,
    pub run_dir: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Git {
    pub binary: PathBuf,
}

impl Default for Git {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("/usr/bin/git"),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
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

#[derive(Debug, Default, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Json,
    Pretty,
}

/// Transport spike: config-defined repositories and one static token.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spike {
    /// Password accepted for HTTP Basic auth (any non-empty username).
    #[serde(deserialize_with = "secret")]
    pub token: Secret<String>,
    #[serde(default)]
    pub repos: Vec<SpikeRepo>,
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

fn secret<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Secret<String>, D::Error> {
    let s = String::deserialize(d)?;
    if s.len() < 16 {
        return Err(serde::de::Error::custom(
            "token must be at least 16 characters",
        ));
    }
    Ok(Secret::new(s))
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let cfg: Self = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        for (what, p) in [
            ("storage.data_dir", &cfg.storage.data_dir),
            ("storage.run_dir", &cfg.storage.run_dir),
            ("git.binary", &cfg.git.binary),
        ] {
            if !p.is_absolute() {
                return Err(format!("{what} must be an absolute path"));
            }
        }
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        [server]
        [storage]
        data_dir = "/d"
        run_dir = "/r"
        [spike]
        token = "0123456789abcdef"
        [[spike.repos]]
        id = "0192f1a2-3b4c-7d5e-8f60-718293a4b5c6"
        namespace = "alice"
        name = "demo"
    "#;

    #[test]
    fn parses_minimal_config() {
        let cfg: Config = toml::from_str(MINIMAL).unwrap();
        assert_eq!(cfg.spike.repos[0].default_branch, "main");
        assert!(!format!("{cfg:?}").contains("0123456789abcdef"));
    }

    #[test]
    fn unknown_keys_and_bad_names_are_errors() {
        let unknown = MINIMAL.replace("[server]", "[server]\nlisten_typo = 1");
        let err = toml::from_str::<Config>(&unknown).unwrap_err().to_string();
        assert!(err.contains("listen_typo"), "{err}");
        let reserved = MINIMAL.replace("\"alice\"", "\"admin\"");
        assert!(toml::from_str::<Config>(&reserved).is_err());
        let short = MINIMAL.replace("0123456789abcdef", "short");
        assert!(toml::from_str::<Config>(&short).is_err());
    }
}
