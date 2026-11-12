<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Installing Slath

Slath is one binary, `slath`. It needs PostgreSQL (15 or newer), Git (before
1.0, the newest stable release; `slath config check` tells you if yours is too
old), and OpenSSH for Git over SSH. There are two supported ways to run it:
[containers with Docker Compose](#docker-compose) (or [Podman](#podman)) and
[systemd on a Linux host](#systemd).

## Docker Compose

`deploy/compose/` runs PostgreSQL, Slath, and sshd from the published image
`ghcr.io/slath-org/slath`, which contains a pinned Git and OpenSSH. Nothing
is built on your host. The `sshd` container mounts only the IPC socket's
volume, not the data volume; see [Users and permissions](#users-and-permissions).

1. Get the four files of the release you install (or the signed
   `slath-compose-<version>.tar.gz` from the release; see
   [Verifying a release](verify.md)):

   ```sh
   mkdir slath && cd slath
   for f in compose.yml Caddyfile postgres-init.sh .env.example; do
       curl -fsSLO "https://raw.githubusercontent.com/slath-org/slath/v0.3.0/deploy/compose/$f"
   done
   cp .env.example .env
   ```

2. Set the two values in `.env`: `SLATH_DB_PASSWORD` (any long random
   string, for example from `openssl rand -hex 24`) and `SLATH_PUBLIC_URL`,
   the address people use. Everything else has a default; the file lists
   the optional settings.

3. Start everything:

   ```sh
   docker compose up -d
   ```

   On first start, the `init` service creates the master key in the data
   volume, and `slath` sets up the database.

4. Create the first administrator (use your own name; names such as
   `admin` and `root` are reserved). A random password is printed once:

   ```sh
   docker compose run --rm slath admin create-user --admin \
       --username ada --email ada@example.org
   ```

5. Open `SLATH_PUBLIC_URL` (by default <http://localhost:3000>) and sign
   in. Administration needs two-factor authentication: set it up under
   **Settings → Security**, then sign out and in again with a code (see
   [Accounts and sign-in](accounts.md)). Create a repository. Clone it over
   HTTPS with a personal access token as the password, or over SSH after
   adding your key under **Settings → SSH keys**:

   ```sh
   git clone http://localhost:3000/ada/project.git
   git clone ssh://git@localhost:2222/ada/project.git
   ```

6. Back up the master key now and keep it apart from your backups:

   ```sh
   docker compose cp slath:/var/lib/slath/master.key ./master.key
   ```

To update, set `SLATH_VERSION` in `.env` to the new release (or get the
new release's `compose.yml`) and run `docker compose up -d`; see
[Upgrading](upgrading.md). Slath backs up before each update into the
`backups` volume and keeps the newest two.

### TLS

For a public server, keep HTTP published on `127.0.0.1` (the default) and
let the `tls` profile run Caddy in front, which gets a certificate for the
host in `SLATH_PUBLIC_URL`:

```sh
# in .env
SLATH_PUBLIC_URL=https://git.example.org
```

```sh
docker compose --profile tls up -d
```

On a private network, set `SLATH_TLS=internal` to use Caddy's own CA (the
clients must trust its root certificate, printed by `docker compose exec
caddy cat /data/caddy/pki/authorities/local/root.crt`), or
`SLATH_TLS=files` with your own `cert.pem` and `key.pem` in `./certs`
(`SLATH_CERTS_DIR`). `slath doctor` warns before a certificate expires.

Caddy has a fixed address in the Compose network, and Slath trusts exactly
that address to report the client's address (`X-Forwarded-For`). If the
network's default subnet `172.29.79.0/24` is taken on your host, set
`SLATH_SUBNET`, `SLATH_DYNAMIC_RANGE`, and `SLATH_PROXY_IP` in `.env`.

### Health checks

Each container reports its health to Docker (and to Podman): `postgres`
when it accepts connections, `slath` when `/-/readyz` answers (database,
data directory, and background workers are fine), and `sshd` when it greets
SSH clients. The checks run `slath healthcheck` inside the container, so
the image needs no curl.

```sh
docker compose ps              # STATUS shows (healthy) or (unhealthy)
docker events --filter event=health_status
```

A first start that applies an update may take a while; the check allows
ten minutes before it counts failures. For monitoring from outside,
`GET /-/readyz` answers 200 when Slath can serve and 503 when it can't, and
`slath doctor --output json` explains why.

### Configuration

The image reads `/etc/slath/slath.toml` (see `deploy/docker/slath.toml`).
Override any key with an environment variable `SLATH__<SECTION>__<KEY>`, or
mount your own file there. `docker compose exec slath slath config check`
validates the result.

## Podman

The same Compose file runs on rootless Podman (4.7 or newer; tested with
Podman 6.1 and Docker Compose 5.6 as its provider). Every step under
[Docker Compose](#docker-compose) applies, with `podman compose` in place
of `docker compose`. Set up once, as the user who runs Slath:

```sh
systemctl --user enable --now podman.socket   # the API that Compose talks to
loginctl enable-linger "$USER"                # keep running after logout
systemctl --user enable podman-restart.service  # start the containers at boot
```

Then, in the directory with the four files:

```sh
podman compose up -d
podman compose ps            # (healthy) next to each service
```

- **Compose provider:** `podman compose` runs `docker-compose` (Compose v2)
  if it's installed, else `podman-compose`. Use Compose v2: it waits for
  `postgres` to be healthy before starting `slath`, as the file expects.
- **Health checks:** Podman runs them with systemd timers of your user
  session, so they need `systemd --user` (lingering keeps it running).
  `podman healthcheck run <container>` runs one by hand, and `podman events
  --filter event=health_status` follows changes.
- **Ports:** rootless Podman can't bind ports below 1024. HTTP (`3000`) and
  SSH (`2222`) are fine; for the `tls` profile's `80` and `443`, allow them
  with `sudo sysctl net.ipv4.ip_unprivileged_port_start=80` (and in
  `/etc/sysctl.d/`), or put the host's own reverse proxy in front.
- **SELinux:** the Compose file labels its bind mounts (`Caddyfile`,
  `postgres-init.sh`) for container access, so it works with SELinux
  enforcing.
- **Volumes and users:** the containers run as their own users inside your
  user namespace; volumes live under `~/.local/share/containers/storage/`.
  Back them up as described in [Backup](backup.md), not by copying that
  directory while running.
- **If networks fail to start** with `netavark: nftables error: "nft" did
  not return successfully`, the host's nftables doesn't work with
  netavark's firewall rules. Rootless setups don't need them: set
  `firewall_driver = "none"` in the `[network]` section of
  `~/.config/containers/containers.conf`.

## systemd

On a Linux host with systemd, `slath setup` does the installation.

1. Install PostgreSQL (15 or newer, with the contrib modules; on
   RHEL-based systems `postgresql-contrib`), Git, and OpenSSH from your
   distribution, and start PostgreSQL.

2. Install the `slath` binary root-owned (sshd only runs a key lookup
   that nobody else can write). The release binary needs glibc 2.41 or
   newer (Debian 13, Ubuntu 25.04, Fedora 42, or later):

   ```sh
   install -m 0755 slath /usr/local/bin/slath
   ```

   Download it from the release and verify its signature first
   ([Verifying a release](verify.md)). Until release binaries are
   published, build it (`(cd web && npm ci &&
   npm run build) && cargo build --release --locked -p slath`, see
   [docs/dev/setup.md](../dev/setup.md)) or copy it out of the container
   image (`/usr/bin/slath`).

3. See what setup would do, then do it:

   ```sh
   slath setup --check --public-url https://git.example.org
   slath setup --public-url https://git.example.org \
       --admin-username ada --admin-email ada@example.org
   ```

   It creates the service account `slath` and the SSH login `git` (see
   [Users and permissions](#users-and-permissions)), `/etc/slath` with the
   master key, the database URL file with a random password, and the
   configuration, the database role and database (as the local
   `postgres` user, or through `--pg-admin-url`; otherwise it prints the
   two SQL statements to run), the systemd unit and the sshd drop-in. Then
   it checks the result, sets up the schema, creates the administrator
   (the password is printed once), starts `slath.service`, and reloads
   sshd. Without the flags, it asks.

   Running it again changes nothing unless something drifted: it repairs
   missing files and wrong owners or modes, and never rewrites your
   configuration, master key, or database URL.

4. Back up `/etc/slath/master.key` somewhere safe, apart from your
   backups.

5. **Reverse proxy:** forward HTTPS for your public URL to
   `127.0.0.1:3000` and list the proxy's own address in
   `server.trusted_proxies` (the default trusts only `127.0.0.1` and `::1`).

6. Sign in, set up two-factor authentication under **Settings → Security**
   (administration needs it), sign in again with a code, create a
   repository, and clone it over HTTPS and SSH.

If a `git` user already exists for something else, set up by hand with
another name: the files setup writes are `deploy/systemd/` and
`deploy/sshd/slath.conf` in the source; use your name in
`storage.socket_group`, the service's `SupplementaryGroups=`, and the
sshd drop-in.

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

- `slath doctor` checks the installation and says what to fix
  ([Diagnosing an installation](doctor.md)).
- [Backup and restore](backup.md)
- [Upgrading](upgrading.md)
- `slath fsck` checks repositories against the database; `--reconcile`
  repairs refs changed outside Slath.
- Metrics are served on `server.metrics_listen` (Prometheus format); keep
  that address private.
