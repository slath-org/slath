<!-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de> -->
<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Test keys

`signing_ed25519.b64` (the private key, base64-encoded) signs commits and
tags in the `signed` fixture (`make_signed.sh` decodes it); its public half
is `signing_ed25519.pub`. It is a **public, test-only** key: it guards nothing and
must never be trusted anywhere. It is committed so fixture object IDs stay
stable across runs (Ed25519 signatures are deterministic), and encoded only
so that code hosts' secret scanners don't reject pushes of it.
