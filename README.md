<!-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de> -->
<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Slath

Slath is a secure, lightweight, self-hosted code forge where Git and
[Jujutsu](https://jj-vcs.github.io/jj/) users collaborate on equal terms.

> **Early development.** 0.3 is the first release built for others to
> install: prebuilt, signed binaries and a container image, and every later
> release updates from it. Before 1.0, minor releases may still change
> interfaces; read the changelog before updating.

A small installation needs only Slath, PostgreSQL, Git, and OpenSSH.

Website and documentation: <https://slath.org>

## What works in 0.3

- **Merge requests:** from branches and forks, with a revision per push,
  changes tracked across rebases by jj change IDs or their content (no client
  setup for Git or jj), diffs and interdiffs, review threads, suggestions,
  approvals, requested reviewers, a merge box that names what's missing, and
  merging on the server by fast-forward, rebase, merge commit, or squash.
- **Forks:** fork a repository, keep it in sync, and open merge requests from
  it; maintainers can push to a fork's merge request branch.
- **Checks and automation:** commit statuses with line annotations, required
  checks in branch protection, webhooks, deploy keys, and bot accounts with
  their own tokens, limits, and a kill switch.
- **Push protection:** pushes that contain secrets are rejected before
  anything is stored, with time-limited allowances and an allowlist for
  false positives.
- **Accounts:** local users with passwords, passkeys, two-factor
  authentication, sessions, sudo mode, SSH keys, scoped personal access
  tokens, avatars, and several confirmed email addresses. Sign-in with OpenID
  Connect providers, registration by invitation or open sign-up, and password
  reset by email.
- **Organizations and repositories:** owners, members, teams, collaborators
  with read/write/maintain/admin roles, public, internal, and private
  repositories, branch protection, sensitive paths, renames with redirects,
  archiving, a trash for deleted repositories, and import from another Git
  server.
- **Issues and notifications:** a Markdown editor, labels, milestones,
  assignees, references, closing keywords, attachments, search, and an inbox
  and email for issues and merge requests.
- **Git and Jujutsu:** clone and push over HTTPS and SSH (protocol v0 and v2),
  with a push policy enforced in managed hooks. Every merge request flow is
  tested with Git-made, jj-made, and mixed stacks.
- **Code browser:** files, READMEs, highlighted code and diffs, blame,
  history, commits, branches, tags, raw files, and archive downloads.
- **API:** a REST API under `/api/v1` with an OpenAPI document.
- **Operations:** `slath setup` for systemd hosts and Docker Compose with
  the published image; updates by installing the new version and
  restarting; scheduled backups, `slath doctor`, an audit log, encrypted
  secrets with key rotation, repository maintenance, and Prometheus
  metrics.
- **Web UI:** built for WCAG 2.2 AA and checked with axe, with light and dark
  themes; it works with mouse, touch, and keyboard and loads nothing from
  other sites. Every installation serves its own documentation.

Not yet: CI and an MCP endpoint for agents. See the
[roadmap](https://slath.org) for what comes next.

## Installing

Follow [docs/admin/install.md](docs/admin/install.md) for Docker Compose or
systemd, [docs/admin/verify.md](docs/admin/verify.md) to verify the
downloads, [docs/admin/backup.md](docs/admin/backup.md) for backups, and
[docs/admin/upgrading.md](docs/admin/upgrading.md) for updates. See
[CHANGELOG.md](CHANGELOG.md) for everything in this release. Every
installation serves its own version's documentation at `/-/docs/`
(**Help** in the footer), also without internet access.

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

The server is licensed under the
[GNU Affero General Public License v3.0 or later](LICENSE). See
[LICENSES/](LICENSES/README.md) for the licenses of other parts.
Hosting repositories on Slath doesn't affect their licenses.
