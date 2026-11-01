<!-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de> -->
<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Contributing to Slath

Thanks for your interest in Slath. Slath is in early development, so
interfaces, configuration, and storage change often. Please read this guide
before you open an issue or pull request.

Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).
How decisions are made is described in [GOVERNANCE.md](GOVERNANCE.md).

## Reporting problems

- **Security vulnerabilities:** don't open a public issue. Follow
  [SECURITY.md](SECURITY.md).
- **Bugs:** open an issue with the bug report template. Include the Slath
  commit, your Git and jj versions, and steps to reproduce.
- **Feature ideas:** open an issue with the feature request template and
  describe the problem before the solution.

## Before you start on a change

Small fixes (typos, obvious bugs, missing tests) can go straight to a pull
request. For anything larger, such as a new feature, a new dependency, a
database migration, or a change to the API, the configuration, or the Git or
SSH behavior, open an issue first and wait until a maintainer agrees with the
approach. That saves you from writing code that can't be merged.

Slath deliberately keeps its scope small. A feature that fits a large forge
may still be declined here.

## Development setup

See [docs/dev/setup.md](docs/dev/setup.md). In short:

```sh
just setup && just dev-db && just tools && just check
```

`just check` runs every check a pull request must pass: formatting, Clippy,
tests, the offline SQL query data, `cargo deny`, the license dependency
check, the helper script tests, and the static guards.

## Pull requests

- Keep each pull request to one logical change. Split refactoring from
  behavior changes.
- `just check` must pass.
- Every behavior change comes with tests that fail without it. Bug fixes
  start with a test that reproduces the bug.
- Changes that affect Git or jj clients need a test in `tests/compat/` that
  runs a real client. Security-relevant changes need a test that shows
  the attack fails.
- If you change a SQL query, run `just sqlx-prepare` and commit the updated
  `.sqlx/` data.
- Add a line under `[Unreleased]` in [CHANGELOG.md](CHANGELOG.md) for every
  change users or operators will notice. Each entry is one line of at most
  200 characters; split bigger changes into several entries.
- Update the documentation the change affects.

### Rules the guards enforce

`just guards` checks rules that are easy to miss in review:

- Every source file starts with an SPDX header naming its license, for
  example `// SPDX-License-Identifier: AGPL-3.0-or-later` in AGPL crates.
  New files also get an `SPDX-FileCopyrightText` line with your name.
- Only the hardened runner in `crates/git/src/runner.rs` starts Git
  processes, and server code never runs a shell.
- Templates don't use the `|safe` filter, and pages load no resources from
  third-party URLs.

### Commits

Write commit messages in the imperative ("Add …", "Fix …"), with a subject
line of at most about 60 characters. Use the body to explain why the change
is needed and anything a reviewer wouldn't see from the diff.

## Developer Certificate of Origin

Slath uses the [Developer Certificate of Origin](https://developercertificate.org/)
(DCO) instead of a contributor license agreement. By adding a
`Signed-off-by` line to a commit, you certify that you wrote the change or
otherwise have the right to submit it under the project's licenses:

```text
Signed-off-by: Your Name <you@example.org>
```

`git commit -s` adds the line for you. Use your real name or the name you're
publicly known by. Commits without a sign-off can't be merged. To sign off
commits you've already made, run `git rebase --signoff main` and force-push
your branch.

## Licensing of contributions

Your contribution is licensed under the license of the files it changes
(inbound equals outbound):

| Part | License |
|---|---|
| Server, web UI, and crates unless stated otherwise | AGPL-3.0-or-later |
| Client SDK crates and the runner protocol crate | Apache-2.0 |
| Documentation | CC-BY-SA-4.0 |

See [LICENSES/](LICENSES/README.md) for details. New dependencies must have a
license that `deny.toml` allows, and Apache-2.0 crates must not depend on
AGPL crates.

## AI-assisted contributions

You may use AI tools, such as code assistants and LLM agents, to help with
your contribution, and you don't need to say that you did. The same rules
apply as for any other contribution:

- **You're responsible for all of it.** You must understand every line you
  submit and be able to explain and change it during review.
- **The DCO still applies.** By signing off, you certify that you have the
  right to submit the change under the project's licenses. Make sure your
  tool doesn't reproduce code under incompatible terms.
- **Check the result.** Run `just check`, read the diff yourself, and make
  sure the tests actually test the behavior.
- **Respect reviewers' time.** Pull requests, issues, or review comments that
  look generated and unchecked, such as changes the author doesn't understand,
  invented APIs, or unverified bug reports, are closed without detailed
  review.

## Getting help

Ask in the issue or pull request you're working on. If you're stuck, say
what you tried. That's always welcome.
