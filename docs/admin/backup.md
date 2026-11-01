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

The database is read in a single snapshot, and Git refs are copied before
objects, so every copied ref points to objects that are in the backup.
Pushes that land while the backup runs may or may not be included; a restore
reconciles the database with the refs it finds.

`--include-secrets` keeps passwords in the copied configuration. Use it only
if the backup is stored as safely as the server itself.

**The master key is never part of a backup.** Back up the file named by
`secrets.master_key_file` (or `<data_dir>/instance.key`) separately and keep
it apart from the backups. Without it, a restored instance signs everyone
out and can't decrypt stored secrets.

No `pg_dump` is needed, so the PostgreSQL client version never has to match
the server.

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
objects, checks each repository's connectivity, reconciles the database with
the restored refs, and prints the counts next to those in the manifest.

The restore refuses to run against a database or data directory that isn't
empty.
