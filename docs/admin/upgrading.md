<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Upgrading Slath

Back up first ([Backup and restore](backup.md)). Then follow the section for
the release you're upgrading from.

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
