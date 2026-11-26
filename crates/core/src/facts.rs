// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The parts of a diff's facts that need no Git: which files are
//! dependency manifests and lockfiles, the packages a lockfile names, how
//! two package sets differ, and which paths look like tests. Everything is
//! decided from file names and contents alone, so Git-made and jj-made
//! changes with the same trees get the same facts.

use std::collections::{BTreeMap, BTreeSet};

/// A package ecosystem whose manifests and lockfiles are recognized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ecosystem {
    Cargo,
    Npm,
    Go,
    Python,
    Ruby,
    Php,
    Java,
    Nix,
}

impl Ecosystem {
    /// The name used in the API.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Npm => "npm",
            Self::Go => "go",
            Self::Python => "python",
            Self::Ruby => "ruby",
            Self::Php => "php",
            Self::Java => "java",
            Self::Nix => "nix",
        }
    }
}

/// How the packages of a recognized file are read, if at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `[[package]]` tables with `name` and `version` (Cargo, Poetry, uv).
    TomlPackages,
    /// `package-lock.json` and `npm-shrinkwrap.json`, any lockfile version.
    NpmLock,
    /// The `require` directives of `go.mod`.
    GoMod,
    /// Requirement lines of `requirements*.txt`.
    Requirements,
    /// Recognized, but its packages aren't compared.
    Unparsed,
}

/// The ecosystem and format of a dependency file, by its file name.
#[must_use]
pub fn dependency_file(path: &[u8]) -> Option<(Ecosystem, Format)> {
    use Ecosystem as E;
    use Format as F;
    let name = path.rsplit(|&b| b == b'/').next().unwrap_or(path);
    let name = std::str::from_utf8(name).ok()?;
    Some(match name {
        "Cargo.lock" => (E::Cargo, F::TomlPackages),
        "Cargo.toml" => (E::Cargo, F::Unparsed),
        "package-lock.json" | "npm-shrinkwrap.json" => (E::Npm, F::NpmLock),
        "package.json" | "yarn.lock" | "pnpm-lock.yaml" => (E::Npm, F::Unparsed),
        "go.mod" => (E::Go, F::GoMod),
        "go.sum" => (E::Go, F::Unparsed),
        "poetry.lock" | "uv.lock" => (E::Python, F::TomlPackages),
        "pyproject.toml" | "Pipfile" | "Pipfile.lock" => (E::Python, F::Unparsed),
        n if n.starts_with("requirements") && n.ends_with(".txt") => (E::Python, F::Requirements),
        "Gemfile" | "Gemfile.lock" => (E::Ruby, F::Unparsed),
        "composer.json" | "composer.lock" => (E::Php, F::Unparsed),
        "pom.xml" | "build.gradle" | "build.gradle.kts" | "gradle.lockfile" => {
            (E::Java, F::Unparsed)
        }
        "flake.lock" => (E::Nix, F::Unparsed),
        _ => return None,
    })
}

/// Package names with the versions a file names for each (an empty
/// version where it names none).
pub type Packages = BTreeMap<String, BTreeSet<String>>;

/// A file whose packages couldn't be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ParseError(String);

/// The packages `data` names, read as `format`. [`Format::Unparsed`] names
/// none.
///
/// # Errors
/// When `data` isn't a valid file of its format.
pub fn packages(format: Format, data: &[u8]) -> Result<Packages, ParseError> {
    let text = std::str::from_utf8(data).map_err(|_| ParseError("not UTF-8".to_owned()))?;
    let mut out = Packages::new();
    match format {
        Format::TomlPackages => toml_packages(text, &mut out)?,
        Format::NpmLock => npm_packages(text, &mut out)?,
        Format::GoMod => go_packages(text, &mut out),
        Format::Requirements => requirement_packages(text, &mut out),
        Format::Unparsed => {}
    }
    Ok(out)
}

fn add(out: &mut Packages, name: &str, version: &str) {
    out.entry(name.to_owned())
        .or_default()
        .insert(version.to_owned());
}

fn toml_packages(text: &str, out: &mut Packages) -> Result<(), ParseError> {
    #[derive(serde::Deserialize)]
    struct File {
        #[serde(default)]
        package: Vec<Package>,
    }
    #[derive(serde::Deserialize)]
    struct Package {
        name: String,
        #[serde(default)]
        version: Option<String>,
    }
    let f: File = toml::from_str(text).map_err(|e| ParseError(e.message().to_owned()))?;
    for p in f.package {
        add(out, &p.name, p.version.as_deref().unwrap_or(""));
    }
    Ok(())
}

fn npm_packages(text: &str, out: &mut Packages) -> Result<(), ParseError> {
    fn nested(deps: &serde_json::Map<String, serde_json::Value>, out: &mut Packages) {
        for (name, d) in deps {
            if let Some(v) = d.get("version").and_then(|v| v.as_str()) {
                add(out, name, v);
            }
            if let Some(more) = d.get("dependencies").and_then(|v| v.as_object()) {
                nested(more, out);
            }
        }
    }
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| ParseError(e.to_string()))?;
    // Lockfile versions 2 and 3 list every installed package by its path.
    if let Some(pkgs) = v.get("packages").and_then(|p| p.as_object()) {
        for (path, p) in pkgs {
            let Some(i) = path.rfind("node_modules/") else {
                continue; // the root project, or a workspace folder
            };
            let name = &path[i + "node_modules/".len()..];
            if let Some(version) = p.get("version").and_then(|v| v.as_str()) {
                add(out, name, version);
            }
        }
        return Ok(());
    }
    // Version 1 nests them.
    if let Some(deps) = v.get("dependencies").and_then(|d| d.as_object()) {
        nested(deps, out);
        return Ok(());
    }
    if v.is_object() {
        Ok(())
    } else {
        Err(ParseError("not a JSON object".to_owned()))
    }
}

fn go_packages(text: &str, out: &mut Packages) {
    let mut block = false;
    for line in text.lines() {
        let line = line.split("//").next().unwrap_or("").trim();
        if block {
            if line == ")" {
                block = false;
            } else if let [name, version, ..] = line.split_whitespace().collect::<Vec<_>>()[..] {
                add(out, name, version);
            }
            continue;
        }
        let Some(rest) = line.strip_prefix("require") else {
            continue;
        };
        if !rest.starts_with([' ', '\t', '(']) {
            continue;
        }
        let rest = rest.trim();
        if rest == "(" {
            block = true;
        } else if let [name, version, ..] = rest.split_whitespace().collect::<Vec<_>>()[..] {
            add(out, name, version);
        }
    }
}

/// A Python package name as PEP 503 compares them: lowercase, with runs of
/// `-`, `_`, and `.` as one `-`.
fn python_name(name: &str) -> String {
    let mut out = String::new();
    let mut sep = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            sep = true;
        } else {
            if sep && !out.is_empty() {
                out.push('-');
            }
            sep = false;
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

fn requirement_packages(text: &str, out: &mut Packages) {
    for line in text.lines() {
        // A comment starts a line or follows whitespace.
        let line = match line.find(" #").or_else(|| line.find("\t#")) {
            Some(i) => &line[..i],
            None => line,
        };
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
            continue;
        }
        // Environment markers and hashes don't name the package.
        let line = line.split(';').next().unwrap_or("").trim();
        let end = line
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
            .unwrap_or(line.len());
        let (name, rest) = line.split_at(end);
        if name.is_empty() || name.starts_with('.') {
            continue; // a URL or a path
        }
        // Extras such as `name[socks]` don't change the package.
        let rest = match rest.trim_start().strip_prefix('[') {
            Some(r) => r.split_once(']').map_or("", |(_, after)| after),
            None => rest,
        };
        let spec = rest.split_whitespace().next().unwrap_or("").trim();
        let version = spec
            .strip_prefix("===")
            .or_else(|| spec.strip_prefix("=="))
            .unwrap_or(spec);
        add(out, &python_name(name), version);
    }
}

/// A package with one version.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Package {
    pub name: String,
    pub version: String,
}

/// A package whose only version changed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Update {
    pub name: String,
    pub from: String,
    pub to: String,
}

/// How two package sets differ.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct PackageChanges {
    pub added: Vec<Package>,
    pub removed: Vec<Package>,
    pub updated: Vec<Update>,
}

impl PackageChanges {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.updated.is_empty()
    }
}

/// What changed from `old` to `new`, by package name: a package with one
/// version on each side that differs is updated; otherwise the versions
/// only one side has are added or removed.
#[must_use]
pub fn compare(old: &Packages, new: &Packages) -> PackageChanges {
    let mut c = PackageChanges::default();
    let none = BTreeSet::new();
    let names: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    for name in names {
        let a = old.get(name).unwrap_or(&none);
        let b = new.get(name).unwrap_or(&none);
        if a == b {
            continue;
        }
        let (av, bv): (Vec<&String>, Vec<&String>) = (a.iter().collect(), b.iter().collect());
        if let ([from], [to]) = (&av[..], &bv[..]) {
            c.updated.push(Update {
                name: name.clone(),
                from: (*from).clone(),
                to: (*to).clone(),
            });
            continue;
        }
        let one = |version: &String| Package {
            name: name.clone(),
            version: version.clone(),
        };
        c.removed.extend(a.difference(b).map(one));
        c.added.extend(b.difference(a).map(one));
    }
    c
}

/// Whether `path` looks like a test: a directory named `test`, `tests`,
/// `__tests__`, `spec`, or `specs` (in any case), or a file name of the
/// usual test patterns of common languages.
#[must_use]
pub fn is_test_path(path: &[u8]) -> bool {
    let Ok(path) = std::str::from_utf8(path) else {
        return false;
    };
    let mut parts: Vec<&str> = path.split('/').collect();
    let Some(name) = parts.pop() else {
        return false;
    };
    if parts.iter().any(|d| {
        ["test", "tests", "__tests__", "spec", "specs"]
            .iter()
            .any(|t| d.eq_ignore_ascii_case(t))
    }) {
        return true;
    }
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    if ext.is_empty() {
        return false;
    }
    stem.ends_with("_test")
        || stem.ends_with("_tests")
        || stem.ends_with(".test")
        || stem.ends_with(".spec")
        || (ext == "py" && stem.starts_with("test_"))
        || (ext == "rb" && stem.ends_with("_spec"))
        || (matches!(ext, "java" | "kt") && (stem.ends_with("Test") || stem.ends_with("Tests")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[(&str, &str)]) -> Packages {
        let mut p = Packages::new();
        for (n, v) in items {
            add(&mut p, n, v);
        }
        p
    }

    #[test]
    fn files_are_recognized_by_name() {
        assert_eq!(
            dependency_file(b"a/b/Cargo.lock"),
            Some((Ecosystem::Cargo, Format::TomlPackages))
        );
        assert_eq!(
            dependency_file(b"requirements-dev.txt"),
            Some((Ecosystem::Python, Format::Requirements))
        );
        assert_eq!(
            dependency_file(b"web/package.json"),
            Some((Ecosystem::Npm, Format::Unparsed))
        );
        assert_eq!(dependency_file(b"src/Cargo.lock.rs"), None);
        assert_eq!(dependency_file(b"cargo.lock"), None);
        assert_eq!(dependency_file(b"notes.txt"), None);
    }

    #[test]
    fn toml_lockfiles() {
        let lock = "version = 4\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.210\"\n\n\
                    [[package]]\nname = \"syn\"\nversion = \"1.0.109\"\n\n\
                    [[package]]\nname = \"syn\"\nversion = \"2.0.87\"\n\n\
                    [[package]]\nname = \"local\"\n";
        let p = packages(Format::TomlPackages, lock.as_bytes()).unwrap();
        assert_eq!(
            p,
            set(&[
                ("serde", "1.0.210"),
                ("syn", "1.0.109"),
                ("syn", "2.0.87"),
                ("local", ""),
            ])
        );
        assert!(packages(Format::TomlPackages, b"[[package]\n").is_err());
        assert!(packages(Format::TomlPackages, b"\xff").is_err());
    }

    #[test]
    fn npm_lockfiles_of_every_version() {
        let v3 = r#"{"lockfileVersion":3,"packages":{
            "":{"name":"app","version":"1.0.0"},
            "node_modules/left-pad":{"version":"1.3.0"},
            "node_modules/@scope/x":{"version":"2.0.0"},
            "node_modules/a/node_modules/left-pad":{"version":"1.1.0"},
            "packages/web":{"version":"0.1.0"},
            "node_modules/web":{"link":true,"resolved":"packages/web"}}}"#;
        assert_eq!(
            packages(Format::NpmLock, v3.as_bytes()).unwrap(),
            set(&[
                ("left-pad", "1.3.0"),
                ("left-pad", "1.1.0"),
                ("@scope/x", "2.0.0"),
            ])
        );
        let v1 = r#"{"lockfileVersion":1,"dependencies":{
            "a":{"version":"1.0.0","dependencies":{"b":{"version":"2.0.0"}}}}}"#;
        assert_eq!(
            packages(Format::NpmLock, v1.as_bytes()).unwrap(),
            set(&[("a", "1.0.0"), ("b", "2.0.0")])
        );
        assert!(packages(Format::NpmLock, b"[1]").is_err());
        assert!(packages(Format::NpmLock, b"{").is_err());
    }

    #[test]
    fn go_mod_requires() {
        let m = "module example.com/m\n\ngo 1.23\n\n\
                 require golang.org/x/text v0.14.0\n\
                 require (\n\tgithub.com/a/b v1.2.3 // indirect\n\n\tgithub.com/c/d v0.0.1\n)\n\
                 replace github.com/a/b => ../b\n\
                 requirement-ish v9\n";
        assert_eq!(
            packages(Format::GoMod, m.as_bytes()).unwrap(),
            set(&[
                ("golang.org/x/text", "v0.14.0"),
                ("github.com/a/b", "v1.2.3"),
                ("github.com/c/d", "v0.0.1"),
            ])
        );
    }

    #[test]
    fn requirement_lines() {
        let r = "# pinned\nDjango==4.2.1  # web\nrequests[socks]>=2.31\n\
                 Zope.Interface===6.0\n-r other.txt\n--hash=sha256:00\n\
                 pytest ; python_version > \"3.8\"\n./local/pkg\n\n";
        assert_eq!(
            packages(Format::Requirements, r.as_bytes()).unwrap(),
            set(&[
                ("django", "4.2.1"),
                ("requests", ">=2.31"),
                ("zope-interface", "6.0"),
                ("pytest", ""),
            ])
        );
    }

    #[test]
    fn package_sets_compare_by_name() {
        let old = set(&[("a", "1"), ("b", "1"), ("c", "1"), ("d", "1"), ("d", "2")]);
        let new = set(&[("a", "1"), ("b", "2"), ("e", "1"), ("d", "2"), ("d", "3")]);
        let c = compare(&old, &new);
        assert_eq!(
            c.updated,
            vec![Update {
                name: "b".into(),
                from: "1".into(),
                to: "2".into()
            }]
        );
        let names = |v: &[Package]| -> Vec<String> {
            v.iter()
                .map(|p| format!("{}@{}", p.name, p.version))
                .collect()
        };
        assert_eq!(names(&c.removed), ["c@1", "d@1"]);
        assert_eq!(names(&c.added), ["d@3", "e@1"]);
        assert!(compare(&old, &old).is_empty());
    }

    #[test]
    fn test_paths() {
        for p in [
            "tests/compat/a.rs",
            "src/Test/x.cs",
            "web/__tests__/a.ts",
            "spec/models/user_spec.rb",
            "pkg/server_test.go",
            "crates/x/src/lib_tests.rs",
            "test_api.py",
            "web/src/a.test.ts",
            "web/src/a.spec.tsx",
            "src/main/java/FooTest.java",
            "src/FooTests.kt",
        ] {
            assert!(is_test_path(p.as_bytes()), "{p}");
        }
        for p in [
            "src/testing.rs",
            "attest.py",
            "contest/a.rs",
            "latest_test",
            "README.md",
            "src/Testament.java",
            "docs/spec.md",
        ] {
            assert!(!is_test_path(p.as_bytes()), "{p}");
        }
    }
}
