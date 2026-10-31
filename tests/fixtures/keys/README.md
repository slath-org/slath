<!-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de> -->
<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Test keys

`signing_ed25519` signs commits and tags in the `signed` fixture
(`make_signed.sh`). It is a **public, test-only** key: it guards nothing and
must never be trusted anywhere. It is committed so fixture object IDs stay
stable across runs (Ed25519 signatures are deterministic).
