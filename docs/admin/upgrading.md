<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Upgrading Slath

You can upgrade directly from any earlier release from 0.3.0 on to the
current one; there's no need to install the releases in between.
`slath upgrade --check` prints the notes of every release you skip.

Releases 0.1.x and 0.2.0 were previews and can't be upgraded; see
[Coming from a preview](#coming-from-a-preview).

## With `slath upgrade`

Install the new binary, then see what would happen. This changes nothing
and is safe to run at any time:

```sh
slath upgrade --check
```

It runs the preflight checks (PostgreSQL and Git versions, free disk space
for the backup, that no other upgrade is running, the configuration,
whether the data directory is writable, and whether your current release
can upgrade directly), and prints the upgrade notes of every release you
skip, including manual steps and background migrations.

Then upgrade:

```sh
slath upgrade
```

1. If any preflight check fails, it stops before changing anything.
2. A running server switches to read-only maintenance: pages show a
   banner, saving and pushing are refused with a message, and browsing and
   cloning keep working.
3. It backs up the database and repositories into
   `<data_dir>/backups/pre-upgrade-<from>-<to>-<time>` (`--backup-dir` puts
   it elsewhere; `--no-backup` skips it, which isn't recommended).
4. It applies the schema migrations one by one under a lock and shows each.
5. It refreshes the Git hooks and registers background migrations, which
   the server runs while it's up (see below).
6. It checks the result and records the upgrade under **Administration →
   Upgrades**.

Then restart `slath serve` with the new version. The read-only mode ends
when it starts.

If a required manual step is listed in the upgrade notes, do it first and
confirm it with `--accept-action <id>`. Pass `--yes` to skip the
confirmation question (scripts, agents), and `--output json` for
machine-readable output.

**If the upgrade is interrupted** (power loss, a killed process, a failed
migration), run `slath upgrade` again: every step can be repeated and it
continues where it stopped. The output says which backup to restore if you
would rather go back.

**Going back** to the previous version means restoring the pre-upgrade
backup with the previous binary ([Backup and restore](backup.md));
downgrades aren't supported otherwise. An older binary refuses to start on
an upgraded database and says which backup to restore.

### Containers

The image starts with `slath serve --migrate`. It runs the same preflight
and refuses to migrate (and to start) when a check fails or the upgrade
notes list a required manual step; the log says what to do. It takes a
backup only if you set `SLATH__UPGRADE__AUTO_BACKUP=true` and give the
backup directory a volume. The recommended way is to upgrade explicitly,
which always backs up:

```sh
docker compose pull   # or build
docker compose run --rm slath slath upgrade
docker compose up -d
```

### Background migrations

Long data migrations run in the background in small batches while Slath
is up, so the upgrade itself stays short. **Administration → Upgrades** and
`slath admin background-migrations list` show progress and the estimated
time left; you can pause, resume, or retry them there. Features that need
one show a notice until it has finished.

## Configuration across releases

A configuration file written for an earlier release keeps working. When a
release renames a key, the old name still works for at least two minor
releases (and within the same major version): Slath maps it to the new name
and logs a warning, and `slath config check` lists it. To update the file:

```sh
slath config migrate /etc/slath/slath.toml
```

It rewrites deprecated keys to their replacements, keeps your comments, and
saves the previous file as `slath.toml.bak`. Running it again changes
nothing. Keys set through `SLATH__…` environment variables are mapped the
same way, but you have to rename those yourself.

## Coming from a preview

The 0.1 and 0.2 releases were previews. Their databases can't be upgraded:
`slath upgrade`, `slath migrate`, `slath serve`, and `slath backup restore`
refuse them and say so. To move to this version:

1. Install it as a new instance, with an empty database and data
   directory ([Installing Slath](install.md)), next to the preview.
2. Create the users and organizations again, and bring each repository over
   with **Import a repository** (`/-/import`, from the preview's HTTPS
   URL, with a token for private repositories) or with
   `git push --mirror`.
3. Create new access tokens: tokens from the previews don't work any more.
   Users add their SSH keys again.

Issues, settings, and other data of the preview aren't carried over.
