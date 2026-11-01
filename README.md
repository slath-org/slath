<!-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de> -->
<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Slath

Slath is a secure, lightweight, self-hosted code forge where Git and
[Jujutsu](https://jj-vcs.github.io/jj/) users collaborate on equal terms.

> **Early development.** 0.1 is the first release, for trying Slath out on a
> small team. Before 1.0, minor releases may change configuration, storage,
> and interfaces; read the changelog before upgrading.

A small installation needs only Slath, PostgreSQL, Git, and OpenSSH.

Website and documentation: <https://slath.org>

## What works in 0.1

- **Accounts:** local users with passwords, sessions, sudo mode, SSH keys, and
  scoped personal access tokens. Administrators manage users from the web UI.
- **Organizations and repositories:** owners, members, collaborators with
  read/write/maintain/admin roles, public and private repositories, renames
  with redirects, archiving, and a trash for deleted repositories.
- **Git and Jujutsu:** clone and push over HTTPS and SSH (protocol v0 and v2),
  with a push policy enforced in managed hooks. jj and plain Git clients are
  tested against every release.
- **Code browser:** files, READMEs, highlighted code, history, commits with
  diffs, branches, tags, raw files, and archive downloads.
- **API:** a REST API under `/api/v1` with an OpenAPI document.
- **Operations:** an audit log, backup and restore, repository maintenance,
  Prometheus metrics, a container image, and Docker Compose and systemd setups.
- **Web UI:** built for WCAG 2.2 AA and checked with axe, with light and dark
  themes; it works with mouse, touch, and keyboard and loads nothing from
  other sites.

Not yet: issues, merge requests, CI, webhooks, and single sign-on. See the
[roadmap](https://slath.org) for what comes next.

## Installing

Follow [docs/admin/install.md](docs/admin/install.md) for Docker Compose or
systemd, and [docs/admin/backup.md](docs/admin/backup.md) for backups. See
[CHANGELOG.md](CHANGELOG.md) for everything in this release.

## Development

See [docs/dev/setup.md](docs/dev/setup.md). In short:

```sh
just setup && just dev-db && just tools && just check
```

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) first;
all commits need a DCO sign-off. Everyone taking part follows the
[Code of Conduct](CODE_OF_CONDUCT.md), and [GOVERNANCE.md](GOVERNANCE.md)
describes how decisions are made. Report security vulnerabilities privately
as described in [SECURITY.md](SECURITY.md).

## License

Copyright © 2026 Erich Seifert. The server is licensed under the
[GNU Affero General Public License v3.0 or later](LICENSE). See
[LICENSES/](LICENSES/README.md) for the licenses of other parts.
Hosting repositories on Slath doesn't affect their licenses.
