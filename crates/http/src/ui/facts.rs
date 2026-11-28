// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Facts panel of a merge request's and a change's page: the size of
//! the diff, then one entry for each fact that applies, and nothing for
//! those that don't.

use slath_service::facts::Facts;

/// Items shown before the rest folds ("and N more"), as everywhere a
/// short list stands in a small space. The rest folds only when it's at
/// least two items: "and 1 more" would take the place of the item it hides.
const SHOWN: usize = 5;

/// One item of a fact: a path, what about it, or both.
pub struct Item {
    pub path: Option<String>,
    pub note: Option<String>,
}

fn path(p: &str) -> Item {
    Item {
        path: Some(p.to_owned()),
        note: None,
    }
}

fn about(p: &str, note: String) -> Item {
    Item {
        path: Some(p.to_owned()),
        note: Some(note),
    }
}

/// One fact that applies.
pub struct Row {
    pub label: &'static str,
    pub items: Vec<Item>,
}

impl Row {
    /// Where the list folds: all of it when at most one item would fold.
    fn cut(&self) -> usize {
        if self.items.len() <= SHOWN + 1 {
            self.items.len()
        } else {
            SHOWN
        }
    }

    pub fn shown(&self) -> &[Item] {
        &self.items[..self.cut()]
    }

    pub fn rest(&self) -> &[Item] {
        &self.items[self.cut()..]
    }

    pub fn more_label(&self) -> String {
        format!("and {} more", self.rest().len())
    }
}

/// The panel.
pub struct Panel {
    /// "3 files, +40 −2".
    pub size: String,
    pub rows: Vec<Row>,
}

/// What a mode change means, in words.
fn mode_note(old: Option<&str>, new: &str) -> String {
    match (old, new) {
        (None, "100755") => "new executable file".to_owned(),
        (None, "120000") => "new symbolic link".to_owned(),
        (Some("100644"), "100755") => "now executable".to_owned(),
        (Some("100755"), "100644") => "no longer executable".to_owned(),
        (Some(_), "120000") => "now a symbolic link".to_owned(),
        (Some("120000"), _) => "no longer a symbolic link".to_owned(),
        (Some(o), n) => format!("mode {o} to {n}"),
        (None, n) => format!("new file with mode {n}"),
    }
}

fn version(name: &str, v: &str) -> String {
    if v.is_empty() {
        name.to_owned()
    } else {
        format!("{name} {v}")
    }
}

/// The panel for `f`.
#[must_use]
pub fn panel(f: &Facts) -> Panel {
    let s = &f.size;
    let size = format!(
        "{} {}, +{} −{}",
        s.files,
        if s.files == 1 { "file" } else { "files" },
        s.additions,
        s.deletions
    );
    let mut rows = Vec::new();
    let mut row = |label, items: Vec<Item>| {
        if !items.is_empty() {
            rows.push(Row { label, items });
        }
    };
    let paths = |v: &[String]| v.iter().map(|p| path(p)).collect::<Vec<_>>();
    row(
        "Outside the work order's scope",
        paths(f.outside_scope.as_deref().unwrap_or_default()),
    );
    row("Sensitive paths", paths(&f.sensitive_paths));
    row(
        "Path classes",
        f.classes
            .iter()
            .map(|c| Item {
                path: None,
                note: Some(match c.paths.as_slice() {
                    [one] => format!("{} ({one})", c.class),
                    many => format!("{} ({} files)", c.class, many.len()),
                }),
            })
            .collect(),
    );
    let mut deps = Vec::new();
    for d in &f.dependencies {
        let c = &d.changes;
        if !d.packages {
            deps.push(about(&d.path, "changed".to_owned()));
        } else if c.is_empty() {
            deps.push(about(&d.path, "no package changes".to_owned()));
        }
        for p in &c.added {
            deps.push(about(
                &d.path,
                format!("added {}", version(&p.name, &p.version)),
            ));
        }
        for u in &c.updated {
            deps.push(about(
                &d.path,
                format!("updated {} {} to {}", u.name, u.from, u.to),
            ));
        }
        for p in &c.removed {
            deps.push(about(
                &d.path,
                format!("removed {}", version(&p.name, &p.version)),
            ));
        }
    }
    row("Dependencies", deps);
    row("Deleted tests", paths(&f.deleted_tests));
    row("Added binaries", paths(&f.added_binaries));
    row(
        "Mode changes",
        f.mode_changes
            .iter()
            .map(|m| about(&m.path, mode_note(m.old_mode.as_deref(), &m.new_mode)))
            .collect(),
    );
    row(
        "Possible secrets",
        f.secrets
            .iter()
            .map(|x| about(&x.path, format!("line {}: {}", x.line, x.description)))
            .collect(),
    );
    row(
        "Not computed exactly",
        f.incomplete
            .iter()
            .map(|n| Item {
                path: None,
                note: Some(n.clone()),
            })
            .collect(),
    );
    Panel { size, rows }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_facts_that_apply_show() {
        let mut f = Facts::default();
        f.size.files = 1;
        f.size.additions = 2;
        let p = panel(&f);
        assert_eq!(p.size, "1 file, +2 −0");
        assert!(p.rows.is_empty());
        f.outside_scope = Some(Vec::new());
        f.deleted_tests = (0..8).map(|i| format!("t{i}")).collect();
        let p = panel(&f);
        assert_eq!(p.rows.len(), 1);
        assert_eq!(p.rows[0].label, "Deleted tests");
        assert_eq!(p.rows[0].shown().len(), 5);
        assert_eq!(p.rows[0].more_label(), "and 3 more");
        // One more item shows instead of folding alone.
        f.deleted_tests.truncate(6);
        let p = panel(&f);
        assert_eq!((p.rows[0].shown().len(), p.rows[0].rest().len()), (6, 0));
    }

    #[test]
    fn path_classes_name_their_file_or_count() {
        let f = Facts {
            classes: vec![
                slath_service::facts::ClassFact {
                    class: "ci".into(),
                    paths: vec![".github/workflows/a.yml".into()],
                },
                slath_service::facts::ClassFact {
                    class: "migration".into(),
                    paths: vec!["m/1.sql".into(), "m/2.sql".into()],
                },
            ],
            ..Facts::default()
        };
        let p = panel(&f);
        assert_eq!(p.rows[0].label, "Path classes");
        let notes: Vec<_> = p.rows[0]
            .items
            .iter()
            .filter_map(|i| i.note.as_deref())
            .collect();
        assert_eq!(
            notes,
            ["ci (.github/workflows/a.yml)", "migration (2 files)"]
        );
    }

    #[test]
    fn modes_in_words() {
        assert_eq!(mode_note(Some("100644"), "100755"), "now executable");
        assert_eq!(mode_note(Some("100755"), "100644"), "no longer executable");
        assert_eq!(mode_note(None, "120000"), "new symbolic link");
        assert_eq!(mode_note(Some("100644"), "120000"), "now a symbolic link");
    }
}
