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

## Webhooks

Repository administrators and organization owners add webhooks (see the
[user guide](../user/webhooks.md)). Deliveries leave the server from the
job workers; plan for them in outbound firewall rules.

- Only `https://` URLs work, unless `[webhooks] allow_http = true`.
- The address policy is the one imports use: the receiver's name is
  resolved at every attempt, every address must be public, and Slath
  connects only to the checked address (DNS rebinding can't redirect a
  delivery). To deliver to internal receivers, list their networks in
  `[webhooks] allow_networks` (e.g. `["10.20.0.0/16"]`).
- Redirects aren't followed. An attempt may take 10 seconds; failures are
  retried, 8 attempts in all over about two hours, and 50 failed attempts in a
  row switch a webhook off (audit event `webhook.disabled`, and an email
  to the person it acts for).
- At most `limits.webhooks_per_owner` (20) webhooks per repository or
  organization. Deliveries are kept 30 days. Creating, changing, and
  deleting webhooks is audited (`webhook.create`, `webhook.update`,
  `webhook.delete`).
- Webhook secrets are encrypted with the master key; rotating it
  re-encrypts them (see [secrets](secrets.md)).

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

## Forks

Anyone who can read a repository can fork it into their own namespace or
an organization where they can create repositories, with a token
(`repo:write`): `POST /api/v1/repos/{ns}/{repo}/forks` with `namespace`,
and optionally `name` (default: the parent's), `visibility` (default: the
parent's), and `default_branch_only`. The fork exists right away, empty;
its branches and tags are copied in the background, and pushes to it are
refused until `GET /api/v1/repos/{ns}/{repo}/fork` says `done`.

- Only branches and tags are copied, never merge request or internal
  refs. The parent's packs are hardlinked when both are on the same
  filesystem (so a fork takes almost no space or time) and copied
  otherwise; the rest is fetched. The fork shares nothing with its parent
  afterwards: garbage collection or deletion of the parent doesn't affect
  it.
- Objects only the parent's deleted branches reached aren't copied when
  they are loose or in a cruft pack, which is where they are once the
  parent's maintenance ran. Objects of a recent large push whose branch
  was deleted before that can come along until the fork's own maintenance
  removes them; anyone who can read the fork can fetch them by ID.
- A fork can never be more visible than its parent was when it was
  forked: a fork of a private repository stays private, even after the
  parent is deleted.
- When the person who made a fork of a non-public repository loses read
  access to it (a removed collaborator, team, or organization membership,
  or a visibility change), the fork is locked: only site administrators
  see it, under **Administration → Repositories** as "Locked". It's
  unlocked when access returns, and deleted after
  `maintenance.deleted_repo_days` otherwise. Deleting the parent leaves
  its forks alone.

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

For merging merge requests into a protected branch:

- **Approvals needed** (default 1): approvals from people with write
  access, not counting the author, or bots unless **Count approvals by
  bots** is on.
- **Dismiss approvals when the changes change** (on by default): a new
  revision makes earlier approvals stale, unless it's a rebase that keeps
  every change's patch the same.
- **Dismiss approvals on every new revision** (off by default): stricter,
  even a rebase that keeps every patch needs new approvals, since the code
  it builds on changed.
- **Require resolved threads** and **Require the branch to be up to date
  with the target**.

- **Required status checks:** status contexts that must report `success`
  on the commit that lands, one per line (`ci/build`). Add `by` and names
  to count only those reporters' statuses (`ci/build by ci-bot`).
  Statuses from the merge request's author and from bots the author owns
  never count, whatever the rule says.
- **Accept statuses on the merge request's head for rebase and merge
  commits** (on by default). Off, only a fast-forward merge can use the
  head's statuses.

With several matching rules, the highest number of approvals applies, any
rule can turn on a requirement, bots count only if every rule allows it,
and the required checks of all rules apply (with reporter lists, only
reporters on every list count).

A refused push changes nothing: no ref moves and nothing is recorded. Tags
aren't affected by branch rules (moving or deleting a tag needs the
maintain role). Changes to rules are recorded in the audit log.

## Deploy keys

A deploy key is an SSH key of one repository, for a machine rather than a
person: a build server that clones, or a deployment that pushes. Admins of
the repository add them under **Settings → Deploy keys** (in sudo mode) or
through the API with a `repo:write` token:

```sh
curl -H "Authorization: Bearer $TOKEN" -d '{"key": "ssh-ed25519 AAAA… deploy@ci", "can_push": false}' \
  https://slath.example/api/v1/repos/alice/demo/keys
```

- A key is **read-only** unless `can_push` is set. With it, the key pushes
  like a writer: branch protection applies, it can add tags but not move or
  delete them, and it never changes settings.
- A key reaches **its repository only**. Every other repository, public
  ones and forks of this one included, answers "repository not found".
- A key belongs to exactly one person or repository: one already in use
  anywhere is refused without saying where. Generate a key per repository.
- `GET …/keys` lists them with their last use, and `DELETE …/keys/{id}`
  removes one; the next connection with it fails. Adding and removing are
  in the audit log, and actions taken with a key name it there.

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
- The Watch button at the top of every repository page watches it; Unwatch goes
  back to being notified only about issues you take part in. The
  Notifications box in an issue's sidebar shows whether you are subscribed
  and subscribes to or unsubscribes from that issue; unsubscribing silences
  it even if you opened it or watch the repository.
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

### Writing Markdown

Issue descriptions and comments use the Markdown editor:

- **Rich text** shows formatting while you type; **Markdown** shows the
  source with highlighting. The button next to the toolbar switches between
  them, and the choice is remembered. Text the rich editor would rewrite
  (for example setext headings or reference-style links) opens as source.
  Text you don't change is saved exactly as it was.
- The toolbar and its shortcuts (listed under **Shortcuts**) format text;
  Tab always leaves the editor. Typing `@`, `#`, `~`, `%`, or `owner/repo#`
  offers people, issues, labels, and milestones.
- **Preview** shows the text as it will be published.
- Ctrl+Enter sends the form. In a comment, Ctrl+Shift+Enter comments and
  closes the issue (or reopens a closed one); without text it only changes
  the state.
- Files can be attached with the paperclip button, by pasting, or by
  dropping them; links to issues and commits on this instance become short
  references when pasted.
- Unsent text is kept in the browser until the form is sent; signing out
  removes it.
- Without JavaScript, the field is a plain text box.

### Labels, milestones, and assignees

- A new repository starts with the labels bug, enhancement, documentation,
  question, good first issue, help wanted, duplicate, and wontfix, which
  can be changed or deleted like any other.
- The maintain role creates, edits, and deletes labels (name up to 50
  characters, a color, an optional description) and milestones (title up
  to 100 characters, optional due date and description; open or closed)
  on the repository's Labels and Milestones pages. Label names and
  milestone titles are unique per repository regardless of case.
  Deleting a label removes it from all issues; deleting a milestone leaves
  its issues without one.
- The write role applies labels, sets the milestone, and assigns up to 10
  people from the issue's sidebar, which suggests names while you type. Only people who can read the repository
  can be assigned. Each change appears on the issue's timeline.
- The issue list filters by labels, assignee, author, and milestone, and
  sorts by creation or last update. Filters combine: an issue must match
  all of them, including every listed label. The API takes the same
  filters (`labels`, `assignee`, `author`, `milestone`, `sort`) and sets
  `labels`, `assignees`, and `milestone` with `PATCH` on an issue.
