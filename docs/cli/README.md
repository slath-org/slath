<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# JSON output of `slath` commands

With `--output json`, these commands print one JSON document on standard
output, described by a [JSON Schema](https://json-schema.org/) (draft
2020-12) in this directory. Scripts and agents can rely on them: fields
are only added in later releases, never renamed or removed without an
upgrade note.

| Command | Schema |
| --- | --- |
| `slath doctor` | [doctor.schema.json](doctor.schema.json) |
| `slath admin apply` | [admin-apply.schema.json](admin-apply.schema.json) |
| `slath admin create-bot` | [admin-create-bot.schema.json](admin-create-bot.schema.json) |
| `slath admin create-user` | [admin-create-user.schema.json](admin-create-user.schema.json) |
| `slath admin reset-mfa` | [admin-reset-mfa.schema.json](admin-reset-mfa.schema.json) |
| `slath admin send-test-email` | [admin-send-test-email.schema.json](admin-send-test-email.schema.json) |
| `slath admin rotate-key` | [admin-rotate-key.schema.json](admin-rotate-key.schema.json) |
| `slath admin bot` | [admin-bot.schema.json](admin-bot.schema.json) |
| `slath admin background-migrations` | [admin-background-migrations.schema.json](admin-background-migrations.schema.json) |

Every `slath admin` schema allows either the command's result, with
`"ok": true` (or `false` for `rotate-key` when some values remain), or a
failure: `{"ok": false, "error": "…"}`, with a non-zero exit status.
`slath admin gen-key` prints only the key.
