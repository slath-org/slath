<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Repositories

## Importing from another server

**Import a repository** (`/-/import`, linked from the home page and the
new-repository page) creates a repository and copies all branches and
tags of a Git repository on another server into it. With a token
(`repo:write`), `POST /api/v1/repos/import` does the same and answers
with a report that `GET /api/v1/repos/{owner}/{repo}/import` keeps
current (`queued`, `running`, `done` with branch and tag counts, or
`failed` with the reason). The repository's front page shows the progress
and, for administrators, the result.

- Only `https://` URLs work, unless `[import] allow_http = true`. `file://`,
  `ssh://`, `git://`, and other transports are refused, and so is anything
  that looks like a Git option.
- The server name is resolved when the import is requested and again when
  it runs; every address must be public, and Git connects only to the
  checked address. Loopback, private, shared, link-local (including cloud
  metadata), unique-local, multicast, and reserved addresses, IPv6 forms
  that embed such an IPv4 address, and the instance itself are refused.
  To import from an internal server, list its network in
  `[import] allow_networks` (e.g. `["10.20.0.0/16"]`).
- Redirects aren't followed (use the final URL), bundle URIs are ignored,
  every object is checked, and submodules aren't fetched.
- A username and password (or access token) for private sources are used
  once: they travel encrypted, Git reads them from a file only the server
  can read for the duration of the fetch, and they are deleted afterwards.
  They never appear in a process's arguments or in the stored URL.
- An import may take `limits.import_timeout` (1 hour by default) and fetch
  up to `limits.push_max_bytes`. If it fails, delete the repository and
  try again.
- Only Git data is imported, not issues, merge requests, wikis, or
  releases. `[import] enabled = false` turns imports off.

## Visibility

Every repository is **private** (only people with access), **internal**
(every signed-in user of the instance can read it, open issues, and
clone it; signed-out visitors and anonymous Git clients get "not
found"), or **public** (everyone). Owners choose it when creating or
importing a repository and change it under **Settings → General**, which
asks for the password again; the API takes `"visibility": "internal"`.
Internal works like public for everyone who is signed in: it gives no
role, so only collaborators, teams, and organization members can push or
change settings. Raw files and attachments of internal repositories are
never cached by shared caches.

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
  `owner/repo#12` (issues), `@name` (people), `@org/team` (teams),
  `~bug` or `~"needs review"` (labels of the repository), `%v1.0` or
  `%"Q3 cleanup"` (milestones), and commit IDs (7 to 40 hexadecimal
  characters, or `owner/repo@abc1234`). A reference shows the target's
  title and state only to people who can read it; for everyone else it
  stays plain text and reveals nothing. Team references link only for
  members of the organization.
- The editor suggests references while typing. The same suggestions are
  available with a token at
  `GET /api/v1/repos/{owner}/{repo}/references?kind=user|team|item|label|milestone&q=`:
  at most 10, only from repositories the caller can read, never with email
  addresses, and 60 requests a minute per user. People are ranked by taking
  part in the issue (`item=<number>`), then by access to the repository.
  With open registration, someone without access of their own to the
  repository is only offered people who already took part in it. A reference to another
  issue adds "mentioned this in" to that issue, shown only to people who
  can read where the mention was made.
- Titles have up to 256 characters; descriptions and comments are limited
  by `limits.text_max_bytes` (256 KiB by default).

### Search

- `/-/search` (the Search link in the header) finds issues by words in
  their title, description, or comments, and repositories by part of their
  name or description. Issues accept `"exact phrases"`, `-excluded` words,
  and `or`. Each repository's issue list links to a search limited to it.
- Results contain only what the searcher can read: the database filters
  before ranking and paging, so private matches are never counted and
  pages are always full. Deleted and hidden comments don't match.
- The API offers the same with `GET /api/v1/search/issues?q=&repo=` and
  `GET /api/v1/search/repos?q=`. A token without `repo:read` searches like
  an anonymous visitor.
- Search uses PostgreSQL's full-text search with the language-neutral
  `simple` configuration (words are matched exactly, without stemming) and
  the `pg_trgm` extension, which the upgrade enables. It is part of
  PostgreSQL's standard contrib modules.

### Notifications

- People are notified about issues they opened, commented on, or are
  assigned to, when they are mentioned (`@name`, or `@org/team` for the
  team's members), and about every issue in repositories they watch.
  Creating a repository watches it. Nobody is notified about their own
  actions.
- The Watch button on a repository's front page watches it or ignores it
  (then only mentions and assignments notify). The Notifications box in an
  issue's sidebar subscribes to or unsubscribes from that issue.
- Notifications collect in the inbox (`/-/notifications`, one entry per
  issue, marked unread on new activity) and, if the instance sends email
  and the address is confirmed, as one email per event, threaded per issue
  with `List-Id` and a one-click `List-Unsubscribe` link. People turn email
  off under Settings → Notifications.
- Read access is checked when a notification is created, when its email
  is sent, and when the inbox is shown: someone who loses access hears
  nothing more and no longer sees earlier entries. With a token, the API
  (`GET /api/v1/notifications`, `PATCH /api/v1/notifications/{id}`) needs
  `user:read` (`user:write` to change) and shows entries of private
  repositories only with `repo:read`.

### Attachments

- People who can comment can attach files, up to `limits.attachment_max_bytes`
  (25 MiB by default). The API takes them as `multipart/form-data` (a `file`
  part, token scope `issues:write`) at
  `POST /api/v1/repos/{owner}/{repo}/attachments` and answers with the URL and
  ready-made Markdown.
- Attachments are served from `/{owner}/{repo}/attachments/{id}/{name}` to
  people who can read the repository, and from nowhere else: an attachment of
  a private repository is "not found" for everyone else, and an ID only works
  with its own repository.
- PNG, JPEG, GIF, and WebP images (recognized by their content, not their
  name) display inline. Everything else, including SVG, HTML, and PDF, is a
  download, served in a sandbox with `nosniff`.
- Files are stored once per content under `<data_dir>/blobs/`. A file nothing
  uses any more (its repository was deleted, or an avatar was replaced) is
  deleted after `limits.blob_grace_period` (24 hours by default). Files left
  by interrupted uploads are deleted after the same time.

### Avatars

Users (Settings → Profile) and organization owners (organization settings)
can upload a PNG, JPEG, or WebP avatar of up to `limits.avatar_max_bytes`
(5 MiB by default). It is cropped to a square, scaled to 256 × 256 pixels,
and stored as a new PNG, so EXIF data such as GPS coordinates and camera
details is not kept. Without an upload, a pattern generated from the name is
shown. Avatars never come from external services.

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
