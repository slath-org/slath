// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Machine-readable upgrade notes (`upgrade-notes/<version>.toml`), embedded
//! in the binary, so `slath upgrade --check` can show what applies to the
//! actual version jump.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(rust_embed::RustEmbed)]
#[folder = "../../upgrade-notes"]
struct Files;

/// A release version (`major.minor.patch`, optionally `-pre`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Option<String>,
}

impl FromStr for Version {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, Some(p.to_owned())),
            Some(_) => return Err(format!("invalid version {s:?}")),
            None => (s, None),
        };
        let parts: Vec<u64> = core
            .split('.')
            .map(|p| {
                p.parse::<u64>()
                    .map_err(|_| format!("invalid version {s:?}"))
            })
            .collect::<Result<_, _>>()?;
        let [major, minor, patch] = parts[..] else {
            return Err(format!("invalid version {s:?}"));
        };
        Ok(Self {
            major,
            minor,
            patch,
            pre,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(p) = &self.pre {
            write!(f, "-{p}")?;
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl Serialize for Version {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// Whether an action blocks automatic (container) upgrades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Must be done by the operator; `serve --migrate` refuses the jump.
    Required,
    /// Shown, but doesn't block.
    Recommended,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    pub severity: Severity,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackgroundMigration {
    pub name: String,
    pub impact: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeprecationNote {
    pub config_key: String,
    pub replacement: String,
    pub removal: String,
}

/// Minimum versions of external software, when a release raises them.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Requires {
    #[serde(default)]
    pub git: Option<String>,
    #[serde(default)]
    pub postgres: Option<String>,
}

/// The notes of one release.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Notes {
    pub version: Version,
    /// Schema version the release ships (`schema_info.version`).
    pub schema_version: i32,
    /// Oldest release that can upgrade directly to this one.
    pub min_upgrade_from: Version,
    #[serde(default)]
    pub requires: Requires,
    #[serde(default, rename = "action")]
    pub actions: Vec<Action>,
    #[serde(default, rename = "background_migration")]
    pub background_migrations: Vec<BackgroundMigration>,
    #[serde(default, rename = "deprecation")]
    pub deprecations: Vec<DeprecationNote>,
}

/// Parses one notes file; `name` is its file name (`0.2.0.toml`), which
/// must match the version inside.
pub fn parse(name: &str, text: &str) -> Result<Notes, String> {
    let notes: Notes = toml::from_str(text).map_err(|e| format!("{name}: {e}"))?;
    let stem = name.strip_suffix(".toml").unwrap_or(name);
    if stem != notes.version.to_string() {
        return Err(format!(
            "{name}: file name doesn't match version {}",
            notes.version
        ));
    }
    if notes.min_upgrade_from > notes.version {
        return Err(format!(
            "{name}: min_upgrade_from is newer than the release"
        ));
    }
    let mut ids = std::collections::HashSet::new();
    for a in &notes.actions {
        if a.id.is_empty() || a.text.is_empty() || !ids.insert(&a.id) {
            return Err(format!(
                "{name}: action ids must be unique and texts non-empty"
            ));
        }
    }
    Ok(notes)
}

/// Every embedded notes file, oldest first.
pub fn all() -> Result<Vec<Notes>, String> {
    let mut out = Vec::new();
    for name in Files::iter().filter(|n| n.ends_with(".toml")) {
        let file = Files::get(&name).ok_or_else(|| format!("{name}: missing"))?;
        let text = std::str::from_utf8(&file.data).map_err(|_| format!("{name}: not UTF-8"))?;
        out.push(parse(&name, text)?);
    }
    out.sort_by(|a, b| a.version.cmp(&b.version));
    Ok(out)
}

/// The notes relevant to upgrading from `from` to `to`: every release
/// after `from` up to and including `to`.
#[must_use]
pub fn between<'a>(notes: &'a [Notes], from: &Version, to: &Version) -> Vec<&'a Notes> {
    notes
        .iter()
        .filter(|n| &n.version > from && &n.version <= to)
        .collect()
}

/// The release that introduced `schema` (the oldest one shipping it).
#[must_use]
pub fn release_for_schema(notes: &[Notes], schema: i32) -> Option<&Notes> {
    notes.iter().find(|n| n.schema_version == schema)
}

/// The notes of a version jump as text lines for `upgrade --check`.
#[must_use]
pub fn render(notes: &[&Notes]) -> Vec<String> {
    let mut out = Vec::new();
    for n in notes {
        let v = &n.version;
        if let Some(g) = &n.requires.git {
            out.push(format!("v{v}: requires Git {g} or later"));
        }
        if let Some(p) = &n.requires.postgres {
            out.push(format!("v{v}: requires PostgreSQL {p} or later"));
        }
        for a in &n.actions {
            let kind = match a.severity {
                Severity::Required => "REQUIRED",
                Severity::Recommended => "recommended",
            };
            out.push(format!("v{v}: {kind}: {}", a.text));
        }
        for b in &n.background_migrations {
            out.push(format!(
                "v{v}: background migration '{}' ({})",
                b.name, b.impact
            ));
        }
        for d in &n.deprecations {
            out.push(format!(
                "v{v}: configuration key {} is deprecated; use {} (removed in {})",
                d.config_key, d.replacement, d.removal
            ));
        }
    }
    out
}

/// Required manual actions in a version jump (they block `serve --migrate`).
#[must_use]
pub fn required_actions<'a>(notes: &[&'a Notes]) -> Vec<(&'a Version, &'a Action)> {
    notes
        .iter()
        .flat_map(|n| n.actions.iter().map(move |a| (&n.version, a)))
        .filter(|(_, a)| a.severity == Severity::Required)
        .collect()
}

/// This binary's version.
pub fn current() -> Result<Version, String> {
    env!("CARGO_PKG_VERSION").parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn versions_order() {
        assert!(v("0.1.0") < v("0.1.1"));
        assert!(v("0.1.1") < v("0.2.0-rc.1"));
        assert!(v("0.2.0-rc.1") < v("0.2.0"));
        assert!(v("0.10.0") > v("0.9.9"));
        assert_eq!(v("v1.2.3"), v("1.2.3"));
        for bad in ["", "1", "1.2", "1.2.3.4", "a.b.c", "1.2.3-"] {
            assert!(bad.parse::<Version>().is_err(), "{bad}");
        }
    }

    #[test]
    fn embedded_notes_are_valid() {
        let notes = all().unwrap();
        assert!(notes.iter().any(|n| n.version == v("0.3.0")));
        // The 0.1/0.2 previews have no notes: nothing upgrades from them.
        assert!(notes.iter().all(|n| n.version >= v("0.3.0")));
        assert!(
            notes
                .iter()
                .all(|n| n.schema_version > slath_db::BASELINE_SCHEMA)
        );
        // Schemas never go backwards between releases.
        for w in notes.windows(2) {
            assert!(w[0].schema_version <= w[1].schema_version);
        }
        // Every deprecated configuration key is announced in some release.
        for d in slath_core::config::DEPRECATED_KEYS {
            assert!(
                notes
                    .iter()
                    .flat_map(|n| &n.deprecations)
                    .any(|n| n.config_key == d.old && n.replacement == d.new),
                "{} needs a [[deprecation]] entry in upgrade-notes",
                d.old
            );
        }
    }

    #[test]
    fn parse_rejects_mistakes() {
        let ok = "version = \"0.3.0\"\nschema_version = 20\nmin_upgrade_from = \"0.1.0\"\n\
                  [[action]]\nid = \"runner\"\nseverity = \"required\"\ntext = \"Upgrade runners\"\n\
                  [[background_migration]]\nname = \"x\"\nimpact = \"y\"\n\
                  [[deprecation]]\nconfig_key = \"a.b\"\nreplacement = \"a.c\"\nremoval = \"1.0.0\"\n";
        let n = parse("0.3.0.toml", ok).unwrap();
        assert_eq!(n.actions[0].severity, Severity::Required);
        let lines = render(&[&n]);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("REQUIRED: Upgrade runners"));
        assert_eq!(required_actions(&[&n]).len(), 1);
        assert!(parse("0.3.1.toml", ok).is_err());
        assert!(parse("0.3.0.toml", &format!("{ok}bogus = 1\n")).is_err());
        assert!(parse("0.3.0.toml", &ok.replace("required", "maybe")).is_err());
    }

    #[test]
    fn between_selects_the_jump() {
        let mk = |s: &str| Notes {
            version: v(s),
            schema_version: 1,
            min_upgrade_from: v("0.1.0"),
            requires: Requires::default(),
            actions: vec![],
            background_migrations: vec![],
            deprecations: vec![],
        };
        let notes = vec![mk("0.1.0"), mk("0.1.1"), mk("0.2.0"), mk("0.3.0")];
        let got: Vec<String> = between(&notes, &v("0.1.0"), &v("0.2.0"))
            .iter()
            .map(|n| n.version.to_string())
            .collect();
        assert_eq!(got, vec!["0.1.1", "0.2.0"]);
    }
}
