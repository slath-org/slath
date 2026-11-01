<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Upgrading Slath

You can upgrade directly from any earlier release (from 0.1.0 on) to the
current one; there's no need to install the releases in between. Read the
section for each release you skip below, then run `slath upgrade`.

## With `slath upgrade` (from 0.2.0 on)

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
2. A running 0.2.0-or-newer server switches to read-only maintenance: pages
   show a banner, saving and pushing are refused with a message, and
   browsing and cloning keep working. A server older than 0.2.0 must be
   stopped first.
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

## From 0.1.x to 0.2.0

- **Administrators need two-factor authentication.** `[auth]
  require_mfa_for_admins` (on by default) is enforced from 0.2.0 on: an
  administrator who signed in without a second factor is treated like a
  regular user, and their tokens and SSH keys lose administrator powers
  until the account has one. Set it up under **Settings → Security** before
  or right after upgrading, then sign in again with a code. The `slath
  admin` commands keep working. To keep the old behaviour, set
  `require_mfa_for_admins = false` (not recommended).

## Before 0.2.0

Releases before 0.2.0 have no `slath upgrade`. Back up first
([Backup and restore](backup.md)), then follow the section for the release
you're upgrading from.

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

## From 0.1.0 to 0.1.1

Slath 0.1.1 separates the SSH login `git` from the service account `slath`
(see [Users and permissions](install.md#users-and-permissions)). In 0.1.0,
`git` shared `slath`'s UID, so anything running as `git` could read the
database credentials, the master key, and every repository. Git for SSH now
runs inside `slath serve`, and `git` only needs to reach the IPC socket.

### Docker Compose

The new image and `compose.yml` do this for you:

```sh
git fetch --tags && git checkout v0.1.1
cd deploy/compose
docker compose build
docker compose up -d
docker compose exec slath slath config check
```

`git` becomes UID 10002 with its own group. `slath` (UID 10001, unchanged)
hands the IPC socket to that group on start, and the `sshd` container is
recreated without the data volume. Your volumes and data stay as they are.
The image sets `SLATH__STORAGE__SOCKET_GROUP=git` in its environment, so a
`slath.toml` you mount yourself needs no change.

If you keep your own copy of `compose.yml`, remove the `data` volume from
the `sshd` service and add the `tmpfs` entry for `/var/lib/slath` from the
shipped file. Without it, Docker carries the data volume over to the
recreated container, because the image declares `/var/lib/slath` a volume.
Check with `docker compose exec sshd ls /var/lib/slath`, which must list
nothing.

### systemd

Replacing the binary is enough to keep everything working. Without
`storage.socket_group`, the socket stays private to `slath`'s UID, which the
0.1.0 `git` user shares. To separate the two users, which we recommend:

1. Stop Slath:

   ```sh
   systemctl stop slath
   ```

2. Remove the old `git` account. Use plain `userdel`, never `userdel -r`:
   the old account's home directory is the data directory.

   ```sh
   userdel git
   ```

   If `userdel` reports running processes, wait for open SSH sessions to end
   and run it again.

3. Create the new `git` user and group, and add `slath` to the group:

   ```sh
   cp deploy/systemd/sysusers-slath.conf /usr/lib/sysusers.d/slath.conf
   systemd-sysusers
   usermod --password '*' git
   id git     # its own UID, groups: git
   id slath   # groups include git
   ```

4. In `/etc/slath/slath.toml`, under `[storage]`, add:

   ```toml
   socket_group = "git"
   ```

5. Install the new unit (it adds `SupplementaryGroups=git` and allows the
   server to hand the socket to that group) and the new sshd drop-in (it runs
   `slath ssh-keys` as `git`):

   ```sh
   cp deploy/systemd/slath.service /etc/systemd/system/
   cp deploy/sshd/slath.conf /etc/ssh/sshd_config.d/slath.conf
   systemctl daemon-reload
   ```

6. Check that group `git` can't read Slath's files. If the `socket` line
   reports a path, remove the group and world permissions it names (for
   example `chmod o-rwx /var/lib/slath`).

   ```sh
   sudo -u slath slath config check
   ```

7. Start Slath, reload sshd, and clone a repository over SSH:

   ```sh
   systemctl start slath
   systemctl reload sshd
   ```

The steps must happen in this order: the new unit doesn't start until group
`git` exists.

To go back to 0.1.0 afterwards, restore the shared UID as described in the
0.1.0 install guide: the 0.1.0 `ssh-serve` runs Git itself and needs access
to the repositories.
