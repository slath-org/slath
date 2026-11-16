// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Whether the time zone database built into Slath is older than the
//! host's. Slath never uses the host's; a newer one there means a zone's
//! rules changed and times may be off until Slath is updated.

use std::path::Path;

/// The version of the host's database (`2026e`), from
/// `/usr/share/zoneinfo`.
pub fn system_version(zoneinfo: &Path) -> Option<String> {
    if let Ok(v) = std::fs::read_to_string(zoneinfo.join("+VERSION")) {
        let v = v.trim();
        if is_version(v) {
            return Some(v.to_owned());
        }
    }
    let zi = std::fs::read_to_string(zoneinfo.join("tzdata.zi")).ok()?;
    let v = zi.lines().next()?.strip_prefix("# version ")?.trim();
    is_version(v).then(|| v.to_owned())
}

fn is_version(v: &str) -> bool {
    v.len() >= 5
        && v[..4].bytes().all(|b| b.is_ascii_digit())
        && v[4..].bytes().all(|b| b.is_ascii_lowercase())
}

/// Whether version `a` is newer than `b` (`2026f` > `2026e`, `2027a` >
/// `2026z`, `2026aa` > `2026z`).
fn newer(a: &str, b: &str) -> bool {
    let key = |v: &str| (v[..4].to_owned(), v.len(), v[4..].to_owned());
    is_version(a) && is_version(b) && key(a) > key(b)
}

/// `Some(message)` if the host's database is newer than the bundled one.
pub fn stale(zoneinfo: &Path) -> Option<String> {
    let bundled = slath_core::tz::database_version()?;
    let system = system_version(zoneinfo)?;
    newer(&system, bundled).then(|| {
        format!(
            "the built-in time zone database ({bundled}) is older than this host's ({system}); \
             times in zones whose rules changed may be off until Slath is updated"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_year_then_letters() {
        assert!(newer("2026f", "2026e"));
        assert!(newer("2027a", "2026z"));
        assert!(newer("2026aa", "2026z"));
        assert!(!newer("2026e", "2026e"));
        assert!(!newer("2025z", "2026a"));
        assert!(!newer("bogus", "2026a"));
    }

    #[test]
    fn reads_either_file_and_warns_only_when_the_host_is_newer() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(system_version(d.path()), None);
        assert_eq!(stale(d.path()), None);
        std::fs::write(d.path().join("tzdata.zi"), "# version 2099a\n# more\n").unwrap();
        assert_eq!(system_version(d.path()).as_deref(), Some("2099a"));
        assert!(stale(d.path()).unwrap().contains("(2099a)"));
        std::fs::write(d.path().join("+VERSION"), "1999a\n").unwrap();
        assert_eq!(system_version(d.path()).as_deref(), Some("1999a"));
        assert_eq!(stale(d.path()), None);
    }
}
