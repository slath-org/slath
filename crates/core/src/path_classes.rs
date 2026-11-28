// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Path classes: named sets of files, such as `migration` or `ci`, that
//! become facts of a diff and conditions of protection rules. A small
//! built-in set applies everywhere; a repository adds classes, and globs to
//! the built-in ones, in `.slath/paths.toml` on its default branch. Nothing
//! here can shrink a built-in class. Every invalid entry is reported, and a
//! class with an error doesn't apply.

use std::collections::BTreeMap;

use crate::secret_scan::glob_match;
use crate::sensitive_paths;

/// The repository's file, read from the default branch only.
pub const FILE: &str = ".slath/paths.toml";
/// Largest file read, in bytes.
pub const MAX_FILE_BYTES: usize = 64 * 1024;
/// Most classes the file may define.
pub const MAX_CLASSES: usize = 50;
/// Most globs per class in the file.
pub const MAX_GLOBS: usize = 100;
/// Longest class name.
pub const MAX_NAME: usize = 40;

/// The manifests and lockfiles of the dependency fact, as globs; the
/// built-in class `dependencies`. A test keeps them equal to the files
/// [`crate::facts::dependency_file`] recognizes.
pub const DEPENDENCIES: &[&str] = &[
    "**/Cargo.toml",
    "**/Cargo.lock",
    "**/package.json",
    "**/package-lock.json",
    "**/npm-shrinkwrap.json",
    "**/yarn.lock",
    "**/pnpm-lock.yaml",
    "**/go.mod",
    "**/go.sum",
    "**/pyproject.toml",
    "**/requirements*.txt",
    "**/Pipfile",
    "**/Pipfile.lock",
    "**/poetry.lock",
    "**/uv.lock",
    "**/Gemfile",
    "**/Gemfile.lock",
    "**/composer.json",
    "**/composer.lock",
    "**/pom.xml",
    "**/build.gradle",
    "**/build.gradle.kts",
    "**/gradle.lockfile",
    "**/flake.lock",
];

/// Container build and compose files; the built-in class `containers`.
pub const CONTAINERS: &[&str] = &[
    "**/Dockerfile",
    "**/Dockerfile.*",
    "**/*.Dockerfile",
    "**/Containerfile",
    "**/Containerfile.*",
    "**/compose.yaml",
    "**/compose.yml",
    "**/docker-compose.yaml",
    "**/docker-compose.yml",
    "**/.dockerignore",
];

/// The reviewer file, part of the built-in class `owners`.
const REVIEWERS: &str = ".slath/REVIEWERS";

/// The built-in classes by name, with their globs.
#[must_use]
pub fn built_in() -> Vec<(&'static str, Vec<&'static str>)> {
    let mut owners = sensitive_paths::OWNERS.to_vec();
    owners.push(REVIEWERS);
    vec![
        ("ci", sensitive_paths::CI.to_vec()),
        ("containers", CONTAINERS.to_vec()),
        ("dependencies", DEPENDENCIES.to_vec()),
        ("instructions", sensitive_paths::INSTRUCTIONS.to_vec()),
        ("owners", owners),
    ]
}

/// One class in effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Class {
    pub name: String,
    /// Whether Slath defines it; the file can only add to it.
    pub built_in: bool,
    /// Its built-in globs.
    pub built_in_globs: Vec<String>,
    /// The globs the repository's file adds.
    pub file_globs: Vec<String>,
    /// Whether its files fold in diffs.
    pub fold: bool,
}

impl Class {
    /// Every glob of the class, built-in ones first.
    pub fn globs(&self) -> impl Iterator<Item = &str> {
        self.built_in_globs
            .iter()
            .chain(&self.file_globs)
            .map(String::as_str)
    }

    /// Whether `path` (from the repository root) is in the class, ASCII
    /// case-insensitively.
    #[must_use]
    pub fn matches(&self, path: &[u8]) -> bool {
        let path = path.to_ascii_lowercase();
        self.globs()
            .any(|g| glob_match(g.to_ascii_lowercase().as_bytes(), &path))
    }
}

/// What's wrong with the file, and in which class if it's one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassError {
    pub class: Option<String>,
    pub message: String,
}

/// The classes in effect for a repository, and the file's errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathClasses {
    /// Sorted by name.
    classes: Vec<Class>,
    /// Classes the file defines with an error; they don't apply.
    broken: Vec<String>,
    errors: Vec<ClassError>,
}

impl Default for PathClasses {
    fn default() -> Self {
        Self::parse(None)
    }
}

/// Why a class name is refused, if it is.
fn invalid_name(name: &str) -> Option<&'static str> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Some("needs 1 to 40 characters");
    }
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Some("must start with a lowercase letter");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Some("may only have lowercase letters, digits, and -");
    }
    None
}

/// One class table of the file, or why it's refused.
fn parse_table(name: &str, value: &toml::Value) -> Result<(Vec<String>, bool), String> {
    if let Some(why) = invalid_name(name) {
        return Err(format!("The class name {why}."));
    }
    let Some(table) = value.as_table() else {
        return Err("Must be a table with paths = [...].".to_owned());
    };
    let mut globs = Vec::new();
    let mut fold = false;
    for (key, v) in table {
        match key.as_str() {
            "paths" => {
                let Some(list) = v.as_array() else {
                    return Err("paths must be a list of globs.".to_owned());
                };
                if list.len() > MAX_GLOBS {
                    return Err(format!("Has more than {MAX_GLOBS} paths."));
                }
                for g in list {
                    let Some(g) = g.as_str() else {
                        return Err("paths must be a list of globs.".to_owned());
                    };
                    if let Some(why) = sensitive_paths::invalid_glob(g) {
                        return Err(format!("The path {g:?} {why}."));
                    }
                    globs.push(g.to_owned());
                }
            }
            "fold" => {
                fold = v
                    .as_bool()
                    .ok_or_else(|| "fold must be true or false.".to_owned())?;
            }
            other => return Err(format!("Unknown key {other:?}; use paths and fold.")),
        }
    }
    if globs.is_empty() && !table.contains_key("paths") {
        return Err("Needs paths = [...].".to_owned());
    }
    Ok((globs, fold))
}

impl PathClasses {
    /// The classes from the file's text (`None`: no file), over the
    /// built-in ones.
    #[must_use]
    pub fn parse(text: Option<&str>) -> Self {
        let mut map: BTreeMap<String, Class> = built_in()
            .into_iter()
            .map(|(name, globs)| {
                (
                    name.to_owned(),
                    Class {
                        name: name.to_owned(),
                        built_in: true,
                        built_in_globs: globs.into_iter().map(str::to_owned).collect(),
                        file_globs: Vec::new(),
                        fold: false,
                    },
                )
            })
            .collect();
        let mut out = Self {
            classes: Vec::new(),
            broken: Vec::new(),
            errors: Vec::new(),
        };
        if let Some(text) = text {
            out.read_file(text, &mut map);
        }
        out.classes = map.into_values().collect();
        out
    }

    fn read_file(&mut self, text: &str, map: &mut BTreeMap<String, Class>) {
        if text.len() > MAX_FILE_BYTES {
            self.errors.push(ClassError {
                class: None,
                message: format!(
                    "The file is larger than {} KiB; only the built-in classes apply.",
                    MAX_FILE_BYTES / 1024
                ),
            });
            return;
        }
        let table: toml::Table = match toml::from_str(text) {
            Ok(t) => t,
            Err(e) => {
                self.errors.push(ClassError {
                    class: None,
                    message: format!(
                        "The file isn't valid TOML; only the built-in classes apply: {}",
                        e.message()
                    ),
                });
                return;
            }
        };
        if table.len() > MAX_CLASSES {
            self.errors.push(ClassError {
                class: None,
                message: format!(
                    "The file defines more than {MAX_CLASSES} classes; only the built-in \
                     classes apply."
                ),
            });
            return;
        }
        for (name, value) in &table {
            match parse_table(name, value) {
                Ok((globs, fold)) => {
                    let class = map.entry(name.clone()).or_insert_with(|| Class {
                        name: name.clone(),
                        built_in: false,
                        built_in_globs: Vec::new(),
                        file_globs: Vec::new(),
                        fold: false,
                    });
                    class.file_globs = globs;
                    class.fold = fold;
                }
                Err(message) => {
                    self.errors.push(ClassError {
                        class: Some(name.clone()),
                        message,
                    });
                    // A built-in class keeps its own globs.
                    if !map.get(name).is_some_and(|c| c.built_in) {
                        self.broken.push(name.clone());
                    }
                }
            }
        }
    }

    /// The classes in effect, by name.
    #[must_use]
    pub fn classes(&self) -> &[Class] {
        &self.classes
    }

    /// The file's errors, by class name.
    #[must_use]
    pub fn errors(&self) -> &[ClassError] {
        &self.errors
    }

    /// The class named `name`, if it's in effect.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Class> {
        self.classes.iter().find(|c| c.name == name)
    }

    /// Whether the file defines `name` with an error.
    #[must_use]
    pub fn is_broken(&self, name: &str) -> bool {
        self.broken.iter().any(|b| b == name)
    }

    /// The names of the classes `path` is in.
    #[must_use]
    pub fn of(&self, path: &[u8]) -> Vec<&str> {
        self.classes
            .iter()
            .filter(|c| c.matches(path))
            .map(|c| c.name.as_str())
            .collect()
    }

    /// The first folding class `path` is in, if any.
    #[must_use]
    pub fn folding(&self, path: &[u8]) -> Option<&str> {
        self.classes
            .iter()
            .find(|c| c.fold && c.matches(path))
            .map(|c| c.name.as_str())
    }
}

/// A class a path only suggests, proposed from a tree with the globs that
/// would define it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub name: &'static str,
    pub globs: Vec<String>,
}

/// Folder names that suggest database migrations.
const MIGRATION_DIRS: &[&str] = &["migrations", "migrate", "alembic"];
/// Name parts that suggest authentication.
const AUTH_WORDS: &[&str] = &["auth", "login", "oauth", "oidc", "session", "password"];

/// The `migration` and `auth` classes the tree's `paths` suggest, unless
/// `classes` already has one of that name. Globs name the matching folders
/// and files, at most 20 each.
#[must_use]
pub fn proposals<'a>(
    paths: impl IntoIterator<Item = &'a [u8]>,
    classes: &PathClasses,
) -> Vec<Proposal> {
    let mut migration = std::collections::BTreeSet::new();
    let mut auth = std::collections::BTreeSet::new();
    for path in paths {
        let Ok(path) = std::str::from_utf8(path) else {
            continue;
        };
        let parts: Vec<&str> = path.split('/').collect();
        let (dirs, name) = parts.split_at(parts.len().saturating_sub(1));
        if let Some(i) = dirs
            .iter()
            .position(|d| MIGRATION_DIRS.iter().any(|m| d.eq_ignore_ascii_case(m)))
        {
            migration.insert(format!("{}/**", dirs[..=i].join("/")));
        }
        let has_word = |s: &str| {
            let s = s.to_ascii_lowercase();
            AUTH_WORDS.iter().any(|w| s.contains(w))
        };
        if let Some(i) = dirs.iter().position(|d| has_word(d)) {
            auth.insert(format!("{}/**", dirs[..=i].join("/")));
        } else if name.first().is_some_and(|n| has_word(n)) {
            auth.insert(path.to_owned());
        }
    }
    let mut out = Vec::new();
    for (name, globs) in [("auth", auth), ("migration", migration)] {
        if !globs.is_empty() && classes.get(name).is_none() {
            out.push(Proposal {
                name,
                globs: globs.into_iter().take(20).collect(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_classes_apply_without_a_file() {
        let c = PathClasses::default();
        let of = |p: &str| c.of(p.as_bytes());
        assert_eq!(of(".github/workflows/ci.yml"), ["ci"]);
        assert_eq!(of("crates/core/Cargo.toml"), ["dependencies"]);
        assert_eq!(of("requirements-dev.txt"), ["dependencies"]);
        assert_eq!(of("deploy/Dockerfile"), ["containers"]);
        assert_eq!(of("app.Dockerfile"), ["containers"]);
        assert_eq!(of("sub/AGENTS.md"), ["instructions"]);
        assert_eq!(of(".slath/REVIEWERS"), ["owners"]);
        assert_eq!(of("CODEOWNERS"), ["owners"]);
        assert!(of("src/main.rs").is_empty());
        assert!(c.errors().is_empty());
    }

    #[test]
    fn dependency_globs_are_the_dependency_fact_files() {
        for g in DEPENDENCIES {
            let name = g.trim_start_matches("**/").replace('*', "-x");
            assert!(
                crate::facts::dependency_file(name.as_bytes()).is_some(),
                "{g}"
            );
        }
        let c = PathClasses::default();
        for f in [
            "Cargo.lock",
            "a/package-lock.json",
            "requirements.txt",
            "flake.lock",
            "x/build.gradle.kts",
        ] {
            assert!(crate::facts::dependency_file(f.as_bytes()).is_some());
            assert!(c.of(f.as_bytes()).contains(&"dependencies"), "{f}");
        }
    }

    #[test]
    fn the_file_adds_classes_and_extends_built_in_ones() {
        let c = PathClasses::parse(Some(
            r#"
[migration]
paths = ["migrations/**", "db/schema.sql"]

[ci]
paths = ["build/pipeline.yml"]

[api-docs]
paths = ["docs/api/*.json"]
fold = true
"#,
        ));
        assert!(c.errors().is_empty(), "{:?}", c.errors());
        let of = |p: &str| c.of(p.as_bytes());
        assert_eq!(of("migrations/0001.sql"), ["migration"]);
        assert_eq!(of("DB/Schema.SQL"), ["migration"], "ASCII case");
        assert_eq!(of("build/pipeline.yml"), ["ci"]);
        assert_eq!(of(".github/workflows/x.yml"), ["ci"], "built-in globs stay");
        assert_eq!(c.folding(b"docs/api/openapi.json"), Some("api-docs"));
        assert_eq!(c.folding(b"migrations/0001.sql"), None);
        let ci = c.get("ci").unwrap();
        assert!(ci.built_in);
        assert_eq!(ci.file_globs, ["build/pipeline.yml"]);
    }

    #[test]
    fn invalid_entries_are_reported_and_dont_apply() {
        let c = PathClasses::parse(Some(
            r#"
[Migration]
paths = ["m/**"]

[deploy]
paths = ["/abs"]

[ci]
paths = 3

[ok]
paths = ["ok/**"]

[extra]
paths = ["e/**"]
owners = ["alice"]
"#,
        ));
        let errors: Vec<(Option<&str>, &str)> = c
            .errors()
            .iter()
            .map(|e| (e.class.as_deref(), e.message.as_str()))
            .collect();
        assert_eq!(
            errors,
            [
                (
                    Some("Migration"),
                    "The class name must start with a lowercase letter."
                ),
                (Some("ci"), "paths must be a list of globs."),
                (
                    Some("deploy"),
                    "The path \"/abs\" must not start or end with /."
                ),
                (Some("extra"), "Unknown key \"owners\"; use paths and fold."),
            ]
        );
        assert!(c.get("deploy").is_none() && c.is_broken("deploy"));
        assert!(c.get("extra").is_none() && c.is_broken("extra"));
        // A built-in class with an error keeps its own globs.
        assert!(c.get("ci").is_some() && !c.is_broken("ci"));
        assert!(c.of(b".github/workflows/a.yml").contains(&"ci"));
        assert!(c.of(b"ok/a").contains(&"ok"));
    }

    #[test]
    fn an_unreadable_file_leaves_the_built_in_classes() {
        for text in ["not toml [", &"#".repeat(MAX_FILE_BYTES + 1)] {
            let c = PathClasses::parse(Some(text));
            assert_eq!(c.errors().len(), 1);
            assert_eq!(c.errors()[0].class, None);
            assert_eq!(c.classes().len(), built_in().len());
        }
        let many: String = (0..=MAX_CLASSES)
            .map(|i| format!("[c{i}]\npaths = [\"x\"]\n"))
            .collect();
        assert_eq!(PathClasses::parse(Some(&many)).errors().len(), 1);
    }

    #[test]
    fn proposals_come_from_the_tree() {
        let tree: Vec<&[u8]> = vec![
            b"src/main.rs",
            b"db/migrations/0001.sql",
            b"db/migrations/0002.sql",
            b"src/auth/mod.rs",
            b"src/auth/token.rs",
            b"web/login.ts",
        ];
        let none = PathClasses::default();
        let p = proposals(tree.iter().copied(), &none);
        assert_eq!(
            p,
            [
                Proposal {
                    name: "auth",
                    globs: vec!["src/auth/**".into(), "web/login.ts".into()],
                },
                Proposal {
                    name: "migration",
                    globs: vec!["db/migrations/**".into()],
                },
            ]
        );
        // A class the file already defines isn't proposed.
        let has = PathClasses::parse(Some("[migration]\npaths = [\"db/**\"]\n"));
        assert_eq!(proposals(tree.iter().copied(), &has).len(), 1);
        assert!(proposals([b"src/main.rs".as_slice()], &none).is_empty());
    }
}
