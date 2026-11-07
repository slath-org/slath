<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Keeping upgrades safe

Every release must upgrade directly from every earlier release of the same
major version, back to the upgrade baseline 0.3.0. These rules keep that
true. The 0.1/0.2 previews are no upgrade source: their databases (schema
below 100) are refused with guidance, and nothing in the code exists only
for them.

## Upgrade notes

Each release has `upgrade-notes/<version>.toml`. The binary embeds the
files and `slath upgrade --check` prints those between the installed and
the new version. `just upgrade-notes` checks that every release tag has its
file; the release workflow also checks the file of the tag being released.

```toml
version = "0.5.0"
schema_version = 58          # REQUIRED_SCHEMA of the release
min_upgrade_from = "0.3.0"

[requires]                   # optional: raised minimum versions
git = "2.47.0"
postgres = "16"

[[action]]                   # manual steps
id = "runner-upgrade"
severity = "required"        # required: containers refuse to auto-upgrade
text = "Upgrade CI runners to 0.5.0 or later"

[[background_migration]]
name = "mr_change_patch_ids"
impact = "Merge request interdiffs show 'computing' until it finishes"

[[deprecation]]
config_key = "ci.max_parallel"
replacement = "ci.max_concurrent_per_repo"
removal = "2.0.0"
```

Unknown keys are errors. Add to the in-development release's file as you
go; the release task completes it.

## Renaming a configuration key

1. Rename the field in `crates/core/src/config.rs`.
2. Add an entry to `DEPRECATED_KEYS` there (old and new dotted path, the
   release that deprecates it, and the major release that may remove it).
3. Add a matching `[[deprecation]]` to the release's upgrade notes (a unit
   test fails otherwise).

Loading maps the old key with a warning, and `slath config migrate`
rewrites files. The shipped configurations of every release since 0.3.0
are kept in `tests/fixtures/config/` (the release task adds them) and must
keep loading.

## Schema changes

See the migration rules in the data model specification: fast DDL only,
expand and contract over two releases, and backfills as background
migrations.

`migrations/0100_baseline.sql` is the schema of the previews collapsed into
one file, and every supported installation starts there. New migrations
take the next number after the newest file (0101, 0102, …) and set
`schema_info.version` to it; `REQUIRED_SCHEMA` in `crates/db/src/lib.rs` is
that number.

## Testing upgrades

- `tests/ops/upgrade.rs` loads fixtures from `tests/fixtures/schema/` and
  upgrades them: `baseline.sql` (data at the baseline schema, until a
  release since 0.3.0 has its own `v<version>.sql`) and every release
  fixture. `preview/v0.2.0.sql` checks that previews are refused.
- `just upgrade-matrix` builds every release since 0.3.0 from its tag and
  upgrades an instance each of them populated. Without such a release, it
  reports the case as N/A. `SLATH_UPGRADE_FAULT=<step>` makes `slath upgrade`
  abort at a step (`after-maintenance`, `after-backup`,
  `after-first-migration`, `after-migrations`, `after-hooks`), as a kill
  would; the tests then run it again and check the result.
- Changing the per-repository Git config: edit `REPO_CONFIG` and bump
  `REPO_CONFIG_VERSION` in `crates/git/src/storage.rs`. A background
  migration named `repo_config_v<N>` re-applies it to existing repositories.
