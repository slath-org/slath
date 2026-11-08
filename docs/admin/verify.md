<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Verifying a release

Every release from 0.3.0 on is signed. The release workflow signs the
binaries, `SHA256SUMS`, the Compose bundle, and the container image with
Sigstore, using its own GitHub identity; there's no long-lived key. The
signature comes with build provenance: which workflow built the file, from
which tag and commit. Verify before you install, and again before you
update.

A valid signature proves that the file was built by Slath's release
workflow from the release tag. It doesn't prove the code is free of bugs.

## With the GitHub CLI

```sh
v=0.3.0
gh release download "v$v" --repo slath-org/slath --pattern "slath-$v-x86_64-linux"
gh attestation verify "slath-$v-x86_64-linux" --repo slath-org/slath \
    --signer-workflow slath-org/slath/.github/workflows/release.yml \
    --source-ref "refs/tags/v$v"
```

The container image:

```sh
gh attestation verify "oci://ghcr.io/slath-org/slath:$v" --repo slath-org/slath \
    --signer-workflow slath-org/slath/.github/workflows/release.yml \
    --source-ref "refs/tags/v$v"
```

Check `--source-ref` against the version you meant to install: a signature
of an older release is valid too.

## Offline

Each release has a signature bundle, `slath-<version>.sigstore.json`.
Download it together with the files, and Sigstore's trusted root once on
a machine with network access:

```sh
gh release download "v$v" --repo slath-org/slath --pattern "slath-$v.sigstore.json"
gh attestation trusted-root > trusted_root.jsonl
```

Then, on the offline machine:

```sh
gh attestation verify "slath-$v-x86_64-linux" --bundle "slath-$v.sigstore.json" \
    --custom-trusted-root trusted_root.jsonl --repo slath-org/slath \
    --signer-workflow slath-org/slath/.github/workflows/release.yml \
    --source-ref "refs/tags/v$v"
```

## With cosign

```sh
cosign verify-blob-attestation --new-bundle-format \
    --bundle "slath-$v.sigstore.json" --type slsaprovenance1 \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    --certificate-identity "https://github.com/slath-org/slath/.github/workflows/release.yml@refs/tags/v$v" \
    "slath-$v-x86_64-linux"
```

## Checksums

`SHA256SUMS` lists every published file and is signed itself. Once it
verifies, `sha256sum -c --ignore-missing SHA256SUMS` checks the files you
downloaded.
