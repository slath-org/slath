<!-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de> -->
<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Slath

Slath is a secure, lightweight, self-hosted code forge where Git and
[Jujutsu](https://jj-vcs.github.io/jj/) users collaborate on equal terms.

> **Early development.** Slath isn't usable yet. Nothing here is stable:
> configuration, storage layout, and interfaces will change without notice.

A small installation will need only Slath, PostgreSQL, Git, and OpenSSH.

Website and documentation: <https://slath.org>

## What works today

- Git smart HTTP (protocol v0 and v2) and Git over SSH through OpenSSH, for
  repositories listed in the configuration, with jj and plain Git clients.
- Pushes run through managed hooks that fail closed when the server can't
  check them.

See [CHANGELOG.md](CHANGELOG.md) for details.

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
