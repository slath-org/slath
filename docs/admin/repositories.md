<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Repositories

## Branch protection

Repository administrators protect branches under **Settings → Branch
protection** (or `POST /api/v1/repos/{ns}/{repo}/protections`). A rule
names a branch or a pattern: `*` matches any characters except `/`, and
`?` one such character (`main`, `release/*`). When several rules match a
branch, the strictest setting of each applies.

- **Block force pushes** refuses updates that don't contain the previous
  commit. The check runs on the pushed objects before Git accepts them.
- **Block deletion** refuses deleting the branch.
- **Who may push directly:** the write role and up (default), maintain,
  admins, or nobody (changes land only through merge requests).
- **Require merge requests** refuses direct pushes. Repository admins may
  still push; Git tells them so, and each such push is recorded in the
  audit log (`protection.bypass`).

A refused push changes nothing: no ref moves and nothing is recorded. Tags
aren't affected by branch rules (moving or deleting a tag needs the
maintain role). Changes to rules are recorded in the audit log.
