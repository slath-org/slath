<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Releasing

A release is built once and qualified on exactly what was built. Pushing a
tag `vX.Y.Z` runs `.github/workflows/release.yml`:

1. **Preconditions:** `upgrade-notes/X.Y.Z.toml` exists and matches
   `REQUIRED_SCHEMA`, and the newest scheduled runs of the nightly
   workflows (upgrade matrix, OIDC providers, Git/jj matrix) are green. A red nightly
   blocks the release until it's fixed.
2. **Build:** one image per architecture (x86-64 and aarch64, on native
   runners), pushed to `ghcr.io/slath-org/slath` by digest only, and the
   `slath` binary copied out of it with its SHA-256. The binary is built
   on Debian trixie and needs glibc 2.41 or newer.
3. **Qualify** on those artifacts: the test suite and the browser tests
   with `SLATH_BIN` set to the downloaded binary, `scripts/test-compose.sh`
   and `scripts/test-setup.sh` with the image by digest, and the upgrade
   matrix from every earlier release's published binary
   (`SLATH_MATRIX_FROM=releases`).
4. **Gate:** fails unless every job above succeeded; a skipped or
   cancelled job counts as a failure. It writes `report.md`
   (`scripts/release_report.py`): every artifact with its SHA-256, the
   image digests, and each job's test totals.
5. **Sign and publish:** Sigstore signatures with build provenance for the
   binaries, `SHA256SUMS`, the Compose bundle, and the image index, made
   with the workflow's own GitHub identity. They're verified online and
   offline (and a modified file must fail) before the image gets its tags
   `X.Y.Z`, `X.Y`, and `latest` (a pre-release such as `X.Y.Z-rc.1` gets
   only its own tag and is marked as a pre-release) and the GitHub
   release is created with the files, the signature bundle, and the
   report. Users verify as in `docs/admin/verify.md`.
6. **Documentation:** the publish job builds the release's documentation
   with the built image (`slath docs build --published`) and attaches it
   as `slath-docs-X.Y.Z.tar.gz`, listed in `SHA256SUMS` and signed like
   the other files. The website repository's own workflow
   (`.github/workflows/docs.yml` in slath-org/slath.org) picks it up
   within the hour, or at once when started by hand: it checks the
   checksum and that the provenance names this release workflow and tag,
   adds it as `docs/X.Y.Z/` with this release's
   `scripts/publish_docs.py` (`docs/latest/` for the newest release that
   isn't a pre-release, `docs/versions.json`, `docs/versions.html`),
   commits with its own token, and requests a Pages build. No key or
   secret connects the two repositories.

Before the tag:

- A release candidate (`vX.Y.Z-rc.N`) goes through the same workflow and
  needs the same preparation: the workspace version `X.Y.Z-rc.N` and its
  own `upgrade-notes/X.Y.Z-rc.N.toml`. It stays a pre-release: no `X.Y`
  or `latest` image tag, and the website's `docs/latest/` keeps the last
  release. Never reuse a tag after a failed run; delete that pre-release
  and tag the next candidate.

- Finish `upgrade-notes/X.Y.Z.toml` and the CHANGELOG section, bump the
  workspace version and `SLATH_VERSION`'s default in
  `deploy/compose/compose.yml`, and add the release's shipped
  configurations to `tests/fixtures/config/` and its schema fixture
  (`just schema-fixture`).

After the workflow:

- Download the JUnit artifacts and check the milestone's conformance gate
  with `just conformance-report` (the cases file isn't in the repository,
  so CI can't).
- Someone other than the release author installs from the published
  artifacts on both profiles, restores a backup, updates from a previous
  build, and checks the SSH boundary; the results go into the release
  notes.
