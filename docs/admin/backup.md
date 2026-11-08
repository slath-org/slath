<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Backup and restore

## Create a backup

```sh
slath backup create --output /var/backups/slath/2026-11-01
```

The server keeps running. The command writes into an empty (or new)
directory, readable only by the Slath user:

| Path | Contents |
| --- | --- |
| `manifest.json` | Versions, entity counts, tables, and a SHA-256 checksum for every file |
| `config.toml` | Your configuration, with passwords, secrets, and tokens removed |
| `db/*.copy` | One consistent snapshot of the database |
| `repos/<id>.git/` | Refs and objects of every repository |
| `trash/<id>.git/` | The same for deleted repositories that aren't purged yet |
| `blobs/<aa>/<sha256>` | Attachments and uploaded avatars, named by their SHA-256 |

The database is read in a single snapshot, and Git refs are copied before
objects, so every copied ref points to objects that are in the backup.
Pushes that land while the backup runs may or may not be included; a restore
reconciles the database with the refs it finds. Attachment and avatar files
are copied after the snapshot; unused files are only deleted after
`limits.blob_grace_period` (24 hours by default), so every file the snapshot
refers to is still there.

`--include-secrets` keeps passwords in the copied configuration. Use it only
if the backup is stored as safely as the server itself.

**The master key is never part of a backup.** Back up the file named by
`secrets.master_key_file` (or `<data_dir>/instance.key`) separately and keep
it apart from the backups. Without it, a restored instance signs everyone
out and can't decrypt stored secrets.

No `pg_dump` is needed, so the PostgreSQL client version never has to match
the server.

## Scheduled backups

Slath can back itself up every day:

```toml
[backup]
schedule = "03:15"   # daily, in UTC
keep = 7             # newest scheduled backups to keep
```

The server writes `scheduled-<time>` backups into `<data_dir>/backups`
(the `backups` volume in Compose) while it keeps serving, and then deletes
the oldest scheduled ones beyond `keep`. Other backups there, such as the
pre-upgrade ones, aren't touched by this. A failed run leaves no partial
backup; the admin **Health** page and `slath doctor` report it, and the
next day's run tries again.

### Copy backups off the host

Backups on the same disk don't survive the loss of that disk or host. Copy
them somewhere else, for example nightly after the scheduled backup:

```sh
rsync -a --delete /var/lib/slath/backups/ backup-host:/srv/slath-backups/
# or: restic -r sftp:backup-host:/srv/restic backup /var/lib/slath/backups
```

With Compose, the volume is `slath_backups`; `docker run --rm -v
slath_backups:/b:ro -v "$PWD":/out alpine tar -C /b -czf /out/backups.tgz .`
copies it out. Keep the master key apart from these copies, and test a
restore from time to time.

## Restore

1. Install the same or a newer Slath release.
2. Create an empty database and point a configuration file at it and at an
   empty `data_dir`. Copy the master key into place.
3. Run:

   ```sh
   slath backup restore --input /var/backups/slath/2026-11-01
   ```

The restore first checks the whole backup and stops without changing
anything if any file is missing, unlisted, a link, outside the backup, or
fails its checksum. It then loads the database, migrates it to the current
schema, recreates every repository with Slath's own Git configuration
(repository config and hooks are never taken from a backup), copies refs and
objects, copies attachment and avatar files (each must match its name),
checks each repository's connectivity, reconciles the database with
the restored refs, and prints the counts next to those in the manifest.

The restore refuses to run against a database or data directory that isn't
empty.
