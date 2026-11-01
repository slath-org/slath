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

## Issues

Every repository has issues unless they are turned off. Issues and merge
requests share one sequence of numbers per repository, so `#12` is never
ambiguous.

- Anyone who can read the repository and is signed in can open issues and
  comment. Authors edit and close their own issues and edit their own
  comments; the write role edits and closes any issue; the maintain role
  also hides and deletes others' comments (recorded in the audit log).
- Markdown in issues and comments links references: `#12` and
  `owner/repo#12` (issues), `@name` (people), and commit IDs (7 to 40
  hexadecimal characters, or `owner/repo@abc1234`). A reference shows the
  target's title and state only to people who can read it; for everyone
  else it stays plain text and reveals nothing. A reference to another
  issue adds "mentioned this in" to that issue, shown only to people who
  can read where the mention was made.
- Titles have up to 256 characters; descriptions and comments are limited
  by `limits.text_max_bytes` (256 KiB by default).

### Labels, milestones, and assignees

- The maintain role creates, edits, and deletes labels (name up to 50
  characters, a color, an optional description) and milestones (title up
  to 100 characters, optional due date and description; open or closed)
  on the repository's Labels and Milestones pages. Label names and
  milestone titles are unique per repository regardless of case.
  Deleting a label removes it from all issues; deleting a milestone leaves
  its issues without one.
- The write role applies labels, sets the milestone, and assigns up to 10
  people from the issue's sidebar. Only people who can read the repository
  can be assigned. Each change appears on the issue's timeline.
- The issue list filters by labels, assignee, author, and milestone, and
  sorts by creation or last update. Filters combine: an issue must match
  all of them, including every listed label. The API takes the same
  filters (`labels`, `assignee`, `author`, `milestone`, `sort`) and sets
  `labels`, `assignees`, and `milestone` with `PATCH` on an issue.
