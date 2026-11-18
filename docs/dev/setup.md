<!-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de> -->
<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Developer setup

This gets a clean Linux machine from zero to a green `just check`.

## Prerequisites

- Rust via [rustup](https://rustup.rs). `rust-toolchain.toml` pins the version,
  and rustup installs it on first use.
- The newest stable Git release (currently 2.56.0); before 1.0 Slath
  requires it. `scripts/install-git.sh 2.56.0` builds it if your distribution
  lags behind, and `SLATH_TEST_GIT` points the tests at it.
- Python ≥ 3.11 (helper scripts and their tests).
- Node.js ≥ 22 (`web/.nvmrc` names the version CI uses) to build the UI
  bundle that the server embeds. It's needed at build time only.
- Docker (with the compose plugin) or Podman (with `podman compose`) for the
  development database.

## Steps

```sh
cargo install --locked just     # once, if you don't have just yet
just setup                      # installs cargo-nextest, cargo-deny, zizmor, sqlx-cli; creates .env
just dev-db                     # starts PostgreSQL 18 on 127.0.0.1:54329
just tools                      # installs the pinned jj versions for the compat tests
just check                      # UI bundle, fmt, clippy, tests, cargo deny, guards, zizmor
just test-e2e                   # browser tests (Playwright container, all browsers)
```

`cargo install` puts binaries in `~/.cargo/bin`, which must be on `PATH`.

The compatibility tests also need Docker (or Podman, via
`SLATH_CONTAINER_ENGINE=podman`) for the SSH server fixture. Set
`SLATH_SKIP_SSH_TESTS=1` or `SLATH_SKIP_JJ_TESTS=1` to skip those suites.

### Git and jj clients

`tests/compat/versions.toml` pins the client versions with their SHA-256:
the newest stable Git, and the current jj release plus the previous two.
The compatibility tests use `$SLATH_TEST_GIT` (default `/usr/bin/git`) and
every jj in `$SLATH_TEST_JJ` (colon-separated paths; default: the ones
`just tools` installed). `scripts/install-git.sh <version>` builds a pinned
Git; `scripts/install-jj-latest.sh` installs the latest jj release, checked
against the digest GitHub publishes for it.

The nightly workflow `.github/workflows/compat-matrix.yml` runs the whole
client suite with the pinned Git, every pinned jj, and the latest jj
release. It compares Slath's predicted jj IDs with each real jj binary,
and runs a jj without `change-id` headers, mixed Git and jj teams, every
merge request flow for Git-made, jj-made, and mixed stacks, forks, and
tag pushes. Its report (`scripts/compat_report.py`) names each client and
marks a jj release that isn't pinned yet. A red nightly or manual run opens or
comments on the issue labeled `compat-matrix` and blocks the next release;
the next green run closes it. When a new jj release passes, pin it in
`versions.toml` and drop the oldest.

## Documentation

`docs/user`, `docs/admin`, `docs/cli`, `docs/dev`, and the OpenAPI
document are the documentation every binary serves at `/-/docs/`, built
with the theme in `docs/theme/`. Planning documents (`docs/spec`,
`docs/plan`, and the others) are never part of it. Links between
documents use relative `.md` paths (`../user/webhooks.md#events`); a link
to a missing document or heading fails `just check`. To look at the
output as static files:

```sh
cargo run -p slath -- docs build --out target/docs
```

## Configuration

`.env` (copied from `.env.example`, ignored by Git) sets `DATABASE_URL` for
tests and `sqlx`. To use another port, set `SLATH_DEV_DB_PORT` before
`just dev-db` and adjust `DATABASE_URL` to match.

To use Podman instead of Docker when both are installed, set
`SLATH_CONTAINER_ENGINE=podman`.

## Useful recipes

| Recipe | Does |
|---|---|
| `just test -E 'test(name)'` | Run matching tests (any nextest filter works) |
| `just test -p slath-git` | Run one crate's tests |
| `just fmt` | Format all code |
| `just web` | Rebuild the UI bundle after changing `web/src` |
| `just test-e2e -g name` | Run matching browser tests; `E2E_LOCAL=1` runs Playwright without its container |
| `just css-lint` | Check CSS against the design-token rules |
| `just hooks` | Use the optional pre-commit hook: audits the workflows with zizmor when a commit changes `.github/` |
| `just test-backup-restore` | Run the backup and restore drill |
| `docker build -t slath:test . && just test-compose` | Install with Docker Compose as the guide says, then render every page offline |
| `just schema-fixture` | Write the release schema and data fixture |
| `just dev-mail` | Start a mail catcher: SMTP on `127.0.0.1:1025` (`tls = "none"`), inbox at <http://127.0.0.1:8025> |
| `just dev-db-stop` | Stop PostgreSQL, keeping its data |
| `just dev-db-reset` | Delete the development database volume |

## Web UI

The UI is rendered on the server (Askama templates in
`crates/http/templates`), with htmx and small TypeScript islands from
`web/src`. Every page works without JavaScript; scripts only add
conveniences on top.

Browser support:

- Use Baseline Widely available HTML, CSS, and JavaScript features freely.
- Use Newly available or limited features only as progressive enhancement
  (the page still works where they're missing, as with `field-sizing` or
  `text-wrap`) or with a fallback: a polyfill from npm, bundled and loaded
  with a dynamic `import()` only after feature detection, or a small
  fallback of our own.
- Prefer HTML and CSS solutions to JavaScript ones.
- Bundled third-party code is listed in `THIRD-PARTY-NOTICES.md`.

### Layout and components

Pages are built from the components of the design catalog, which a
debug build serves at `/-/design`: one page per family (for example
`/-/design/layout`), each component with its status, its contract
(purpose, markup, variants and states, accessibility, and what it's not
for), and examples of every variant.

Space between components comes only from the layout components
(`web/src/components/layout.css`): a stack places its children at one of
four distances, set by the tokens `--gap-inside`, `--gap-items`,
`--gap-groups`, and `--gap-sections`, which compact density makes one step
smaller. Components set no outer margins of their own. Widths come from
tokens too (`--measure-form`, `--measure-text`, `--measure-narrow`,
`--details-width`). To change a token, edit `web/src/tokens.json` and run
`just tokens`.

A new or changed component comes with its contract and catalog entry in
the same change.

`just guards` checks the templates against the rules
(`scripts/check_weave.py`): every `<details>` is a disclosure, menu,
popover, or reply box; heading levels never skip; two or more fields in a
`fieldset` make a group of fields; no inline styles; every class a page
uses is shown in the catalog, and every catalog entry is used by a page.
Pages not yet built from the components are listed in
`scripts/weave-allowlist.txt`; the list only shrinks, so moving a page
means removing its lines, and the check fails on a line that no longer
applies.

The browser tests measure the same rules on the rendered pages
(`tests/e2e/tests/weave.spec.ts`): on every catalog page, gaps inside the
layout components are the relationship distances in both densities,
headings have more space above than below, and every disclosure shows a
chevron that turns. One page per page group is checked for indicators and
heading levels against `tests/e2e/weave-baseline.json`, which lists what
pages not yet built from the components still break; it only shrinks too.
