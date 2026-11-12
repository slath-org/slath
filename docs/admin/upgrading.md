<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Upgrading Slath

To update, **install the new version and restart Slath**. You can update
directly from any release since 0.3.0; there's no need to install the
releases in between.

- **Docker Compose:** set `SLATH_VERSION` in `.env` to the new release (or
  use the new release's `compose.yml`), then `docker compose up -d`.
- **systemd:** replace `/usr/bin/slath` with the new binary, then
  `systemctl restart slath`.

When it starts, Slath sees that the database is older and updates it
before serving:

1. It runs the preflight checks (see below). If one fails, or the release
   notes list a manual step you must do first, it **refuses to start**,
   logs what to do, and changes nothing. Fix it, or run the old version
   again until you can.
2. It backs up the database and repositories into
   `<data_dir>/backups/pre-upgrade-<from>-<to>-<time>` (the `backups`
   volume in Compose) and then deletes older pre-upgrade backups except the
   newest two (`[upgrade] backups_kept`). Copy backups off the host as
   well. `[upgrade] auto_backup = false` turns this backup off, which isn't
   recommended.
3. It applies the schema migrations, refreshes the Git hooks, registers
   background migrations, checks the result, and records the update under
   **Administration → Upgrades**.

If the start is interrupted, starting again finishes the update.

Releases 0.1.x and 0.2.0 were previews and can't be upgraded; see
[Coming from a preview](#coming-from-a-preview).

### Hearing about new releases

Turn on `[upgrade] check_for_updates = true` to have Slath ask for new
releases once a day. **Administration → Health** and **Upgrades** then say
when a newer release is available and link its release notes. Slath never
installs anything itself. It's off by default, and then Slath makes no
request at all. The check asks GitHub's release list for this project; set
`[upgrade] feed_url` to use another source in the same format.

## With `slath upgrade`

To see what an update would do before you restart, or to update while the
server keeps serving read-only, install the new binary and run `slath
upgrade` yourself. First the preview, which changes nothing and is safe to
run at any time:

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

The same commands run in the container:

```sh
docker compose run --rm slath upgrade --check
docker compose run --rm slath upgrade --accept-action <id>   # if the notes require it
```

### Background migrations

Long data migrations run in the background in small batches while Slath
is up, so the upgrade itself stays short. **Administration → Upgrades** and
`slath admin background-migrations list` show progress and the estimated
time left; you can pause, resume, or retry them there. Features that need
one show a notice until it has finished.

## PostgreSQL

Keep PostgreSQL on the newest minor release of its major version: minor
releases are security and bug fixes, and installing one is just replacing
the binaries (your distribution's packages, or the image) and restarting.
`slath doctor` and `slath upgrade --check` warn when the server lacks the
latest security release Slath knows of.

- **Compose:** `docker compose pull postgres && docker compose up -d`. The
  image tag `postgres:18` follows the newest 18.x.
- **Packages:** update the PostgreSQL packages and restart the service.

After updating to 18.7, 17.12, 16.16, or 15.20 (security release of
2026-11-12), rebuild Slath's GIN indexes, which those releases fix
corruption in. It takes no lock that stops Slath:

```sh
psql -d slath -c 'REINDEX INDEX CONCURRENTLY repositories_name_trgm' \
  -c 'REINDEX INDEX CONCURRENTLY repositories_description_trgm' \
  -c 'REINDEX INDEX CONCURRENTLY issues_search' \
  -c 'REINDEX INDEX CONCURRENTLY merge_requests_search'
```

With Compose: `docker compose exec postgres psql -U slath -d slath -c …`.

**Slath's role is never a superuser.** `slath setup` creates a role that
only owns Slath's database, and so does the Compose file for a new
installation (the superuser `postgres` logs in only through the container's
socket). Then a bug in Slath, or a PostgreSQL flaw that needs a privileged
role, can't take over the database server. `slath doctor` warns
(`postgres.superuser`) if the role is one. An installation from an older
Compose file has `slath` as the image's built-in superuser, which can't be
demoted; move it to a new database with a backup and restore
(`slath backup create`, then `slath backup restore` into a new Compose
installation).

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
