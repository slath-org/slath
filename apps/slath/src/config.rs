// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Loading the configuration: a TOML file overridden by
//! `SLATH__<SECTION>__<KEY>` environment variables, with secrets read from
//! `*_file` keys. Unknown keys are errors that name the key.

use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use figment::value::{Dict, Value};
use slath_core::Secret;
use slath_core::config::{Config, DEPRECATED_KEYS, Deprecation};

/// Default location of the configuration file.
pub const DEFAULT_PATH: &str = "/etc/slath/slath.toml";

/// A loaded and validated configuration.
#[derive(Debug)]
pub struct Loaded {
    pub cfg: Config,
    /// Deprecated keys that were mapped to their replacements.
    pub warnings: Vec<String>,
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
    load_with(path, env, DEPRECATED_KEYS)
}

fn load_with(path: &Path, env: Env, deprecated: &[Deprecation]) -> Result<Loaded, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let merged = Figment::new().merge(Toml::string(&text)).merge(env);
    let mut root: Dict = merged
        .extract()
        .map_err(|e| format!("{}: {}", path.display(), describe(&e)))?;
    let warnings = map_deprecated(&mut root, deprecated);
    let cfg: Config = Figment::from(Serialized::defaults(root))
        .extract()
        .map_err(|e| format!("{}: {}", path.display(), describe(&e)))?;
    validate(&cfg)?;
    Ok(Loaded { cfg, warnings })
}

/// Removes the value at a dotted path.
fn take(root: &mut Dict, path: &str) -> Option<Value> {
    let (parents, last) = match path.rsplit_once('.') {
        Some((p, l)) => (Some(p), l),
        None => (None, path),
    };
    let mut dict = root;
    for part in parents.into_iter().flat_map(|p| p.split('.')) {
        match dict.get_mut(part) {
            Some(Value::Dict(_, d)) => dict = d,
            _ => return None,
        }
    }
    dict.remove(last)
}

fn contains(root: &Dict, path: &str) -> bool {
    let mut dict = root;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        match (dict.get(part), parts.peek()) {
            (Some(_), None) => return true,
            (Some(Value::Dict(_, d)), Some(_)) => dict = d,
            _ => return false,
        }
    }
    false
}

/// Inserts a value at a dotted path, creating tables on the way.
fn put(root: &mut Dict, path: &str, value: Value) {
    let mut dict = root;
    let mut parts: Vec<&str> = path.split('.').collect();
    let Some(last) = parts.pop() else { return };
    for part in parts {
        let entry = dict
            .entry(part.to_owned())
            .or_insert_with(|| Value::from(Dict::new()));
        if !matches!(entry, Value::Dict(..)) {
            *entry = Value::from(Dict::new());
        }
        match entry {
            Value::Dict(_, d) => dict = d,
            _ => return,
        }
    }
    dict.insert(last.to_owned(), value);
}

/// Moves deprecated keys to their replacements. The replacement wins if
/// both are set. Returns one warning per deprecated key found.
fn map_deprecated(root: &mut Dict, deprecated: &[Deprecation]) -> Vec<String> {
    let mut warnings = Vec::new();
    for d in deprecated {
        let Some(value) = take(root, d.old) else {
            continue;
        };
        if contains(root, d.new) {
            warnings.push(format!(
                "{} is deprecated and ignored because {} is also set; remove it (`slath config migrate`)",
                d.old, d.new
            ));
        } else {
            put(root, d.new, value);
            warnings.push(format!(
                "{} is deprecated since {}; use {} (removed in {}; `slath config migrate` rewrites the file)",
                d.old, d.since, d.new, d.removal
            ));
        }
    }
    warnings
}

/// Rewrites a configuration file's deprecated keys to their replacements,
/// keeping comments and formatting. Returns the changes made.
pub fn migrate_text(
    text: &str,
    deprecated: &[Deprecation],
) -> Result<(String, Vec<String>), String> {
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("invalid TOML: {e}"))?;
    let mut changes = Vec::new();
    for d in deprecated {
        let old: Vec<&str> = d.old.split('.').collect();
        let Some(item) = remove_item(doc.as_table_mut(), &old) else {
            continue;
        };
        let new: Vec<&str> = d.new.split('.').collect();
        if get_item(doc.as_table(), &new) {
            changes.push(format!("removed {} ({} is already set)", d.old, d.new));
        } else {
            insert_item(doc.as_table_mut(), &new, item);
            changes.push(format!("{} -> {}", d.old, d.new));
        }
    }
    // Tables emptied by the moves are dropped.
    let empty: Vec<String> = doc
        .as_table()
        .iter()
        .filter(|(_, v)| v.as_table_like().is_some_and(|t| t.is_empty()))
        .map(|(k, _)| k.to_owned())
        .collect();
    if !changes.is_empty() {
        for k in empty {
            doc.as_table_mut().remove(&k);
        }
    }
    Ok((doc.to_string(), changes))
}

fn remove_item(table: &mut toml_edit::Table, path: &[&str]) -> Option<toml_edit::Item> {
    match path {
        [] => None,
        [last] => table.remove(last),
        [first, rest @ ..] => match table.get_mut(first)? {
            toml_edit::Item::Table(t) => remove_item(t, rest),
            toml_edit::Item::Value(toml_edit::Value::InlineTable(t)) => {
                let mut tmp = t.clone().into_table();
                let out = remove_item(&mut tmp, rest);
                *t = tmp.into_inline_table();
                out
            }
            _ => None,
        },
    }
}

fn get_item(table: &toml_edit::Table, path: &[&str]) -> bool {
    match path {
        [] => false,
        [last] => table.contains_key(last),
        [first, rest @ ..] => match table.get(first) {
            Some(toml_edit::Item::Table(t)) => get_item(t, rest),
            Some(toml_edit::Item::Value(toml_edit::Value::InlineTable(t))) => {
                get_item(&t.clone().into_table(), rest)
            }
            _ => false,
        },
    }
}

fn insert_item(table: &mut toml_edit::Table, path: &[&str], item: toml_edit::Item) {
    match path {
        [] => {}
        [last] => {
            table.insert(last, item);
        }
        [first, rest @ ..] => {
            if !table.get(first).is_some_and(toml_edit::Item::is_table) {
                table.insert(first, toml_edit::Item::Table(toml_edit::Table::new()));
            }
            if let Some(toml_edit::Item::Table(t)) = table.get_mut(first) {
                insert_item(t, rest, item);
            }
        }
    }
}

/// `slath config migrate FILE`: writes `FILE.bak`, then the rewritten file.
pub fn migrate_file(path: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (out, changes) = migrate_text(&text, DEPRECATED_KEYS)?;
    if changes.is_empty() {
        return Ok(changes);
    }
    let mut bak = path.as_os_str().to_owned();
    bak.push(".bak");
    std::fs::copy(path, &bak)
        .map_err(|e| format!("cannot write {}: {e}", Path::new(&bak).display()))?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, out).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))?;
    Ok(changes)
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
    if cfg.storage.socket_group.as_deref() == Some("") {
        return Err("storage.socket_group must not be empty".to_owned());
    }
    if cfg.database.url.is_some() && cfg.database.url_file.is_some() {
        return Err("database: set either url or url_file, not both".to_owned());
    }
    if cfg.auth.session_idle_days == 0
        || cfg.auth.session_absolute_days < cfg.auth.session_idle_days
    {
        return Err("auth: session_idle_days must be > 0 and <= session_absolute_days".to_owned());
    }
    if cfg.smtp.enabled() {
        let from = cfg.smtp.from.trim();
        let addr = from
            .rsplit_once('<')
            .map_or(from, |(_, a)| a.trim_end_matches('>'));
        if from.is_empty() || !addr.contains('@') || from.contains(['\r', '\n']) {
            return Err(
                "smtp.from must be a sender address like \"Slath <noreply@example.org>\" when smtp.host is set"
                    .to_owned(),
            );
        }
        if cfg.smtp.username.is_empty() != cfg.smtp.password_file.is_none() {
            return Err("smtp: set both username and password_file, or neither".to_owned());
        }
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

/// The numeric ID of `storage.socket_group`, if set.
pub fn socket_gid(cfg: &Config) -> Result<Option<u32>, String> {
    let Some(group) = &cfg.storage.socket_group else {
        return Ok(None);
    };
    if let Ok(gid) = group.parse::<u32>() {
        return Ok(Some(gid));
    }
    match nix::unistd::Group::from_name(group) {
        Ok(Some(g)) => Ok(Some(g.gid.as_raw())),
        Ok(None) => Err(format!(
            "storage.socket_group: group {group} does not exist"
        )),
        Err(e) => Err(format!("storage.socket_group: cannot look up {group}: {e}")),
    }
}

/// The master key and the old keys kept for rotation. Without
/// `secrets.master_key_file`, a key generated in `data_dir` is used.
pub fn load_keys(
    cfg: &Config,
) -> Result<(slath_auth::MasterKey, Vec<slath_auth::MasterKey>), String> {
    let master = match &cfg.secrets.master_key_file {
        Some(path) => slath_auth::MasterKey::load(path).map_err(|e| e.to_string())?,
        None => {
            let path = cfg.storage.data_dir.join("instance.key");
            tracing::warn!(path = %path.display(), "secrets.master_key_file is not set; using a generated key in data_dir");
            slath_auth::MasterKey::load_or_create(&path).map_err(|e| e.to_string())?
        }
    };
    let old = cfg
        .secrets
        .old_key_files
        .iter()
        .map(|p| slath_auth::MasterKey::load(p).map_err(|e| format!("secrets.old_key_files: {e}")))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((master, old))
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

    const TEST_DEPRECATIONS: &[Deprecation] = &[
        Deprecation {
            old: "ci.max_parallel",
            new: "ci.max_concurrent_per_repo",
            since: "0.2.0",
            removal: "1.0.0",
        },
        Deprecation {
            old: "server.old_listen",
            new: "server.listen",
            since: "0.2.0",
            removal: "1.0.0",
        },
    ];

    #[allow(clippy::result_large_err)] // figment::Jail's closure signature
    fn load_dep(text: &str, env: &[(&str, &str)]) -> Result<Loaded, String> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slath.toml");
        std::fs::write(&path, text).unwrap();
        let prefix = format!("SLATHDEP{}__", std::process::id());
        let mut result = None;
        figment::Jail::expect_with(|jail| {
            for (k, v) in env {
                jail.set_env(format!("{prefix}{k}"), v);
            }
            result = Some(load_with(
                &path,
                Env::prefixed(&prefix).split("__"),
                TEST_DEPRECATIONS,
            ));
            Ok(())
        });
        result.unwrap()
    }

    #[test]
    fn deprecated_keys_are_mapped_with_warnings() {
        let text = format!("{MINIMAL}\n[ci]\nmax_parallel = 7\n");
        let cfg = load_dep(&text, &[]).unwrap();
        assert_eq!(cfg.ci.max_concurrent_per_repo, 7);
        assert_eq!(cfg.warnings.len(), 1);
        assert!(cfg.warnings[0].contains("ci.max_parallel"));
        // Through the environment as well.
        let cfg = load_dep(MINIMAL, &[("CI__MAX_PARALLEL", "5")]).unwrap();
        assert_eq!(cfg.ci.max_concurrent_per_repo, 5);
        // The replacement wins when both are set.
        let both = format!("{MINIMAL}\n[ci]\nmax_parallel = 7\nmax_concurrent_per_repo = 2\n");
        let cfg = load_dep(&both, &[]).unwrap();
        assert_eq!(cfg.ci.max_concurrent_per_repo, 2);
        assert!(cfg.warnings[0].contains("ignored"));
        // Without deprecated keys: no warnings; unknown keys still fail.
        assert!(load_dep(MINIMAL, &[]).unwrap().warnings.is_empty());
        assert!(load_dep(&format!("{MINIMAL}\n[ci]\nbogus = 1\n"), &[]).is_err());
    }

    #[test]
    fn migrate_rewrites_deprecated_keys_and_keeps_comments() {
        let text =
            format!("# my instance\n{MINIMAL}\n[ci]\n# parallel jobs\nmax_parallel = 7 # tuned\n");
        let (out, changes) = migrate_text(&text, TEST_DEPRECATIONS).unwrap();
        assert_eq!(
            changes,
            vec!["ci.max_parallel -> ci.max_concurrent_per_repo"]
        );
        assert!(out.contains("# my instance"));
        assert!(out.contains("max_concurrent_per_repo = 7"));
        assert!(!out.contains("max_parallel"));
        let cfg = load_dep(&out, &[]).unwrap();
        assert!(cfg.warnings.is_empty());
        assert_eq!(cfg.ci.max_concurrent_per_repo, 7);
        // Idempotent.
        let (again, changes) = migrate_text(&out, TEST_DEPRECATIONS).unwrap();
        assert!(changes.is_empty());
        assert_eq!(again, out);
        // Dotted keys and moves into another table.
        let (out, _) = migrate_text(
            "server.public_url = \"https://x.org\"\nserver.old_listen = \"127.0.0.1:1\"\n[storage]\ndata_dir = \"/d\"\nrun_dir = \"/r\"\n",
            TEST_DEPRECATIONS,
        )
        .unwrap();
        let cfg = load_dep(&out, &[]).unwrap();
        assert!(cfg.warnings.is_empty());
        assert_eq!(cfg.server.listen.port(), 1);
    }

    #[test]
    fn smtp_needs_a_sender_and_complete_credentials() {
        let with = |smtp: &str| load_str(&format!("{MINIMAL}\n[smtp]\n{smtp}\n"), &[]);
        assert!(with("host = \"mx\"").unwrap_err().contains("smtp.from"));
        assert!(with("host = \"mx\"\nfrom = \"Slath <a@b.org>\"").is_ok());
        assert!(with("host = \"mx\"\nfrom = \"a@b.org\"\ntls = \"tls\"").is_ok());
        assert!(with("host = \"mx\"\nfrom = \"a@b.org\"\ntls = \"ssl\"").is_err());
        assert!(
            with("host = \"mx\"\nfrom = \"a@b.org\"\nusername = \"u\"")
                .unwrap_err()
                .contains("password_file")
        );
        // Without a host, email is off and nothing else is required.
        assert!(!with("port = 25").unwrap().smtp.enabled());
    }

    /// The configurations of every release since 0.3.0 still load (each
    /// release adds its shipped files to `tests/fixtures/config/`).
    #[test]
    fn release_configs_load_without_errors() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/config");
        let Ok(entries) = std::fs::read_dir(&root) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap();
            load_str(&text, &[]).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        }
    }

    /// The shipped example and container configurations stay valid.
    #[test]
    fn shipped_configs_load() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for file in ["deploy/slath.example.toml", "deploy/docker/slath.toml"] {
            let text = std::fs::read_to_string(root.join(file)).unwrap();
            let cfg = load_str(&text, &[]).unwrap_or_else(|e| panic!("{file}: {e}"));
            assert!(cfg.secrets.master_key_file.is_some(), "{file}");
        }
    }

    #[test]
    fn socket_group_by_name_or_number() {
        assert_eq!(load_str(MINIMAL, &[]).unwrap().storage.socket_group, None);
        let with = |line: &str| format!("{MINIMAL}\n        socket_group = {line}\n");
        for (toml, want) in [
            ("\"git\"", "git"),
            ("\"10002\"", "10002"),
            ("10002", "10002"),
        ] {
            let cfg = load_str(&with(toml), &[]).unwrap();
            assert_eq!(cfg.storage.socket_group.as_deref(), Some(want), "{toml}");
        }
        // Environment overrides of digits arrive as numbers.
        let cfg = load_str(MINIMAL, &[("STORAGE__SOCKET_GROUP", "10002")]).unwrap();
        assert_eq!(cfg.storage.socket_group.as_deref(), Some("10002"));
        assert_eq!(socket_gid(&cfg).unwrap(), Some(10002));
        assert!(
            load_str(&with("\"\""), &[])
                .unwrap_err()
                .contains("socket_group")
        );
        assert!(load_str(&with("-1"), &[]).is_err());
        assert!(socket_gid(&load_str(&with("\"no-such-group-x\""), &[]).unwrap()).is_err());
        // The shipped example names the SSH login's group (the image sets it
        // in its environment, so mounted configurations keep it).
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let text = std::fs::read_to_string(root.join("deploy/slath.example.toml")).unwrap();
        let cfg = load_str(&text, &[]).unwrap();
        assert_eq!(cfg.storage.socket_group.as_deref(), Some("git"));
        let dockerfile = std::fs::read_to_string(root.join("Dockerfile")).unwrap();
        assert!(dockerfile.contains("SLATH__STORAGE__SOCKET_GROUP=git"));
    }

    #[test]
    fn minimal_config_gets_defaults() {
        let cfg = load_str(MINIMAL, &[]).unwrap();
        assert_eq!(cfg.server.public_url.origin(), "https://git.example.org");
        assert_eq!(cfg.limits.diff_max_files, 300);
        assert_eq!(cfg.limits.push_max_bytes.get(), 2 << 30);
        assert_eq!(cfg.git.ssh_port, 22);
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
}
