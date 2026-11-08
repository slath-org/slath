<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Diagnosing an installation

`slath doctor` checks an installation and says what's wrong and what to do
about it. It changes nothing, needs no privileges beyond the service
account's, contacts nothing but the instance's own public URL, and never
prints secrets.

```sh
sudo -u slath slath doctor                  # systemd
docker compose exec slath slath doctor      # Compose
slath doctor --output json                  # for monitoring
```

It exits with status 1 if any check failed; warnings don't change the
status. The JSON output follows `docs/cli/doctor.schema.json`: `ok`, and a
list of checks with `code`, `status` (`ok`, `warn`, `fail`), `detail`, and
`fix`. The codes are stable; alert on them, not on the texts.

| Code | Checks |
|---|---|
| `config.invalid` | The configuration file loads |
| `config.deprecated` | No deprecated keys (`slath config migrate` rewrites them) |
| `git.version` | The configured Git exists and is new enough |
| `storage.writable` | The data directory is writable |
| `ssh.boundary` | The SSH login's group can reach the IPC socket and nothing else: not the data directory, the master key, or the database URL file |
| `db.unreachable` | PostgreSQL answers with the configured URL |
| `keys.unreadable` | The master key (and old keys) load |
| `disk.low` | Free space on the data directory's file system (warning below 15%, failure below 5% or 1 GiB) |
| `backup.none`, `backup.stale` | A backup exists in the backup directory and is at most a week old |
| `schema.state` | The database schema matches this version (behind: a restart updates it) |
| `update.*` | The update preflight, as in `slath upgrade --check`: PostgreSQL and extensions, Git, source version, required manual steps, configuration, data directory, upgrade lock |
| `tls.expiry`, `tls.trust`, `tls.unreachable` | For an `https` public URL: the certificate is valid for at least 14 more days and trusted by the public roots (a private CA gives a warning) |
