<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Installing Slath

Slath is one binary, `slath`. It needs PostgreSQL (15 or newer), Git (before
1.0, the newest stable release; `slath config check` tells you if yours is too
old), and OpenSSH for Git over SSH. There are two supported ways to run it:
[containers with Docker Compose](#docker-compose) and [systemd on a Linux
host](#systemd).

Both start from the release's source:

```sh
git clone --branch v0.2.0 https://github.com/slath-org/slath.git
cd slath
```

## Docker Compose

`deploy/compose/` runs PostgreSQL, Slath, and sshd. The image contains a
pinned Git and OpenSSH. The `sshd` container mounts only the IPC socket's
volume, not the data volume; see [Users and permissions](#users-and-permissions).

1. Get the files and set a database password:

   ```sh
   cd deploy/compose
   cp .env.example .env
   # edit .env: SLATH_DB_PASSWORD (required) and SLATH_PUBLIC_URL
   ```

2. Build the image and start everything:

   ```sh
   docker compose build
   docker compose up -d
   ```

   On first start, the `init` service creates the master key in the data
   volume, and `slath` migrates the database.

3. Create the first administrator (use your own name; names such as
   `admin` and `root` are reserved). Without `--password-stdin`, a random
   password is printed once:

   ```sh
   docker compose exec slath slath admin create-user --admin \
       --username ada --email ada@example.org
   ```

4. Open `SLATH_PUBLIC_URL` (by default <http://localhost:3000>) and sign
   in. Administration needs two-factor authentication: set it up under
   **Settings → Security**, then sign out and in again with a code (see
   [Accounts and sign-in](accounts.md)). Create a repository. Clone it over HTTPS with a personal access token
   as the password, or over SSH after adding your key under **Settings → SSH
   keys**:

   ```sh
   git clone http://localhost:3000/ada/project.git
   git clone ssh://git@localhost:2222/ada/project.git
   ```

5. Back up the master key now and keep it apart from your backups:

   ```sh
   docker compose cp slath:/var/lib/slath/master.key ./master.key
   ```

### TLS

For a public server, publish HTTP only on `127.0.0.1` (the default) and put a
TLS proxy in front. The `tls` profile runs Caddy, which gets a certificate for
`SLATH_DOMAIN` automatically:

```sh
# in .env
SLATH_DOMAIN=git.example.org
SLATH_PUBLIC_URL=https://git.example.org
```

```sh
docker compose --profile tls up -d
```

Let Slath trust the proxy's `X-Forwarded-For` header. List exactly the
compose network's subnet, not a broad private range such as `172.16.0.0/12`:
every address in the list may claim any client address. Look the subnet up
once the stack is running:

```sh
docker network inspect slath_default --format '{{range .IPAM.Config}}{{.Subnet}} {{end}}'
# e.g. 172.19.0.0/16
```

and add it to the `slath` service's environment in `compose.yml`:

```yaml
SLATH__SERVER__TRUSTED_PROXIES: '["172.19.0.0/16"]'
```

Docker keeps a network's subnet until the network is removed (`docker compose
down` removes it). Check the value again after recreating the network.

### Configuration

The image reads `/etc/slath/slath.toml` (see `deploy/docker/slath.toml`).
Override any key with an environment variable `SLATH__<SECTION>__<KEY>`, or
mount your own file there. `docker compose exec slath slath config check`
validates the result.

## systemd

1. **PostgreSQL:** create a database `slath` owned by a role `slath`. The
   contrib modules must be installed (Slath enables `pg_trgm` and `citext`;
   on RHEL-based systems install `postgresql-contrib`).

2. **Users:** a service account `slath`, which owns the data, and the SSH
   login `git`, a separate user (see [Users and
   permissions](#users-and-permissions)). sysusers creates both and adds
   `slath` to group `git`; `usermod` unlocks `git` for key logins (it never
   gets a password):

   ```sh
   cp deploy/systemd/sysusers-slath.conf /usr/lib/sysusers.d/slath.conf
   systemd-sysusers
   usermod --password '*' git
   ```

   If a `git` user already exists for something else, use another name
   (e.g. `slath-git`) here, in `storage.socket_group`, in the service's
   `SupplementaryGroups=`, and in the sshd drop-in.

3. **Binary and configuration.** Build the binary with Rust and Node.js
   (versions in [docs/dev/setup.md](../dev/setup.md)), or copy it out of the
   container image (`/usr/bin/slath`):

   ```sh
   (cd web && npm ci && npm run build)
   cargo build --release --locked -p slath
   install -m 0755 target/release/slath /usr/bin/slath
   install -d -m 0750 -o root -g slath /etc/slath
   install -m 0640 -o root -g slath deploy/slath.example.toml /etc/slath/slath.toml
   # edit /etc/slath/slath.toml: public_url, git.binary, ... (keep
   # storage.socket_group = "git")
   printf 'postgres://slath:PASSWORD@localhost/slath' > /etc/slath/db-url
   chown root:slath /etc/slath/db-url && chmod 0640 /etc/slath/db-url
   slath admin gen-key --output /etc/slath/master.key
   chown slath:slath /etc/slath/master.key
   ```

   Keep a copy of `/etc/slath/master.key` somewhere safe and separate from
   your backups.

4. **Check, migrate, create an administrator.** `config check` also
   confirms that group `git` can't read the data directory, the master key,
   or the database URL file:

   ```sh
   sudo -u slath slath config check
   sudo -u slath slath migrate
   sudo -u slath slath admin create-user --admin --username ada --email ada@example.org
   ```

5. **Start the service:**

   ```sh
   cp deploy/systemd/slath.service /etc/systemd/system/
   systemctl daemon-reload
   systemctl enable --now slath
   ```

6. **sshd:** copy `deploy/sshd/slath.conf` to
   `/etc/ssh/sshd_config.d/slath.conf` and reload sshd. Logins as `git` get
   their keys from `slath ssh-keys` and are limited to Git commands.

7. **Reverse proxy:** forward HTTPS for your `public_url` to
   `127.0.0.1:3000` and list the proxy's own address in
   `server.trusted_proxies` (the default trusts only `127.0.0.1` and `::1`).

8. Sign in, set up two-factor authentication under **Settings → Security**
   (administration needs it), sign in again with a code, create a
   repository, and clone it over HTTPS and SSH.

## Users and permissions

Slath keeps the server and the SSH login apart at the operating-system level:

- `slath` runs `slath serve`, owns the data directory, and reads the
  configuration, the database URL, and the master key. Git runs as `slath`,
  for HTTPS and SSH alike.
- `git` is the login sshd runs `slath ssh-keys` and `slath ssh-serve` as. It
  has its own UID and isn't in group `slath`. Its group may connect to the
  IPC socket (`/run/slath/slath.sock`, mode 0660 in a 0710 directory, both
  handed to `storage.socket_group` by `slath serve`) and nothing else: the
  helpers send the key lookup and the SSH session to the server, which
  authorizes it and runs Git on the session's stdin and stdout.

So a fault in the helpers, or anything else running as `git`, doesn't
expose the database credentials, the master key, or the repositories. With
Docker Compose, the `sshd` container doesn't have them either. Keep it that
way:

- Don't add anyone but `slath` to group `git`, and let nothing but sshd's
  helpers run as `git`: whoever can open the socket can name any SSH key to
  the server.
- Don't add `git` to group `slath`, and don't make the data directory,
  `/etc/slath`, or the secret files readable by group `git` or by everyone.
  `slath config check` reports it if they are.
- Pass the database URL with `database.url_file` (mode 0640, group `slath`)
  rather than in a configuration file others can read.
- The `slath` user must be a member of `storage.socket_group`, or `slath
  serve` can't hand the socket to it and won't start.

## Next steps

- [Backup and restore](backup.md)
- [Upgrading](upgrading.md)
- `slath fsck` checks repositories against the database; `--reconcile`
  repairs refs changed outside Slath.
- Metrics are served on `server.metrics_listen` (Prometheus format); keep
  that address private.
