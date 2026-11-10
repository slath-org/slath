# Changelog

All notable changes to Slath are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Before 1.0.0, minor versions may contain breaking changes.

## [Unreleased]

### Added

- Merge requests (first part): open them through the API from a branch, and each push to the branch records a new revision, fetchable at `refs/merge-requests/<n>/head`. Numbers are shared with issues.
- Merge request pages: a list with state tabs and filters, a compare page that creates them (template, draft, reviewers, assignees, labels, milestone), and the conversation.
- Merge requests show their commits and changes, each revision with a link to what changed since the previous one, and copy-paste checkout commands for Git and jj.
- A "Recently pushed" banner on the repository front page, branch list, and merge request list links to creating a merge request for the branch.
- Merge requests can be marked as draft or ready, closed, reopened, and their deleted source branch restored, all from the page.
- Merge requests track each commit as a change across pushes, using jj change IDs, `Change-Id` trailers, identical patches, or similar content; Git and jj users need no setup.
- Change IDs use jj's format: jj users see the same IDs as in `jj log`, and Git commits get the ID jj would give them.
- Commits with conflicting or duplicate IDs are marked unresolved and block merging until someone picks a change; wrongly matched commits can be split off or attached.
- The merge request timeline sums up each push, such as "rebased onto main, 2 changes modified, 1 added".
- A push to a merge request's branch lists each change as unchanged, updated, inferred, new, or removed, for Git and jj alike; past `[limits] push_feedback_timeout`, only the link.
- Diffs have a file tree, a split view on wide screens, and an option to ignore whitespace; signed-in users' choices are remembered.
- Review threads on lines of a diff (either side) or on the whole merge request, with replies, resolving, and pending comments sent with the review.
- Threads move with their lines when a new revision leaves them unchanged and are marked outdated otherwise; resolved threads can be required for merging.
- Comment on a line or range from the Changes tab by selecting a line number; threads show under their lines, and files can be marked viewed until they change.
- The conversation shows each thread with its code, replies, and state, and filters to unresolved threads or your own comments.
- Merge request pages open with a readiness card: a headline, the reason, and your one next step; each requirement shows as done, waiting, or blocking.
- Returning reviewers see how much is new since their review, marked in the timeline; metadata changes fold into one line, and bot comments are marked as claims.
- Draft, ready, close, and reopen moved to a menu next to the merge request's title; commits show their check results.
- Branch protection can dismiss approvals on every new revision, even after a rebase that keeps every patch.
- A review ledger records how each commit was matched to a change, which approvals were kept or went stale and why, stale statuses, target moves, and removed signatures.
- Each change's page shows how its versions were matched and its history; `GET …/merge-requests/{n}/ledger` returns the ledger.
- Merge requests merge on the server with fast-forward, rebase, merge commit, or squash, using the head you reviewed and retrying when the target moves.
- Rebase merges keep authors, messages, and change IDs, drop signatures, and write each change's ID into commits made with Git, so jj shows the same IDs.
- Conflicts with the target are listed in the merge box with commands for Git and jj; the source branch can be deleted after merging.
- Commit statuses: tools report `pending`, `success`, `failure`, or `error` per context on a commit, and every status shows its reporter.
- Branch protection can require status contexts, optionally only from listed reporters; the author's own and their bots' statuses never count.
- The merge box shows each required check with its reporter, and why a status didn't count.
- Merge requests can be reviewed: comment, approve, or request changes on the current revision; the author can only comment.
- Reviews can be requested from users and teams; requested reviewers find them under Review requests until they review.
- Approvals stay current after a rebase that keeps every patch and go stale after content changes; the timeline notes both.
- A merge box lists each requirement of the target branch with its reason and fix; `GET …/merge-requests/{n}/mergeability` returns it as data.
- Returning reviewers get links to the changes since their last review.
- Branch protection rules set merge request requirements: approvals needed, dismissing stale approvals, resolved threads, being up to date, and whether bots' approvals count.
- Diffs mark the changed characters within changed lines, also in combining marks, emoji, CJK, and right-to-left text; `[limits]` bound the lines and pairs this applies to.
- Files marked generated in `.gitattributes` (`linguist-generated`, `slath-generated`) are collapsed in diffs, and large diffs load file by file as you scroll.
- Each change of a merge request has a page with its versions; two versions compare exactly (same parent) or as a diff of patches or a tree diff (after a rebase).
- "Compare with previous" in the timeline compares two revisions with `git range-diff`, marked heuristic, with a link to the tree diff.
- `GET /api/v1/repos/{ns}/{repo}/merge-requests/{n}/stack` lists the current revision's changes; `POST …/stack/{position}/bind` resolves, splits, or joins them.
- After a push, Git prints a link to create a merge request for the branch, or the link of its open merge request.
- Scheduled daily backups (`[backup] schedule`, `keep`) with retention; failures show on the Health page and in `slath doctor`. The backup guide explains copying backups off the host.
- The Compose TLS profile also works on private networks, with Caddy's internal CA or your own certificate (`SLATH_TLS`).
- `slath doctor` checks an installation (configuration, SSH separation, database, schema, Git, disk, backups, TLS, update readiness) and says what to fix; `--output json` is for monitoring.
- Push protection rejects pushes that contain secrets, such as provider tokens, private keys, passwords in URLs, and Slath tokens, before anything of them is stored. It works the same for Git and jj.
- Rejections name the commit, file, line, and rule, show a shortened excerpt, and explain how to remove the secret with Git or jj.
- Repository administrators choose block, warn, or off for push protection; the site configuration can enforce a minimum and add or disable rules.
- A rejected push links to a page where repository writers allow a false positive for a limited time; the same push then passes.
- `.slath/secret-scan.toml` on the default branch lists paths and fingerprints that push protection lets through, such as test fixtures.
- Rejected pushes are recorded in the audit log with the rule and fingerprint, never the secret; repositories can require an administrator to allow findings.
- Repository administrators can accept all findings of the first push into an empty repository in one step, for history that exists elsewhere.
- Imported repositories are scanned for secrets afterwards; findings are reported to the repository's administrators but don't stop the import.
- Each release publishes a prebuilt `slath` binary for x86-64 and aarch64 Linux and a container image, with SHA-256 digests and a report of the tests run on exactly those files.
- Releases are signed with Sigstore and carry build provenance; `docs/admin/verify.md` shows how to verify them, also offline.
- `slath setup` installs Slath on a systemd host in one command: users, files, database, service, sshd, and the first administrator. `--check` shows what it would do.
- An opt-in update check (`[upgrade] check_for_updates`) shows administrators when a newer release is available; it makes no request unless turned on.
- Notifications cover merge requests: review requests, ready for review, new revisions, comments, reviews, merges, closing, and reopening, in the inbox and by email.
- Drafts send no review requests; requested reviewers hear about a merge request once it's marked ready.
- Fork repositories through the API: branches and tags are copied in the background with hardlinked packs, and the fork stays whole if its parent is deleted.
- A fork can't be more visible than its parent was; pushes to a fork wait until its copy is done.
- Fork from the repository header: choose the owner, name, and visibility, or copy only the default branch. Forks show where they came from, and each repository lists its forks.
- Forks show how far their default branch is ahead of or behind the original's, and Sync fork brings it forward (or, after confirming, discards the fork's own commits).
- A fork of a private repository is locked when its creator loses access to the original, unlocked when access returns, and deleted after the retention period.
- Open merge requests from a fork through the API: the original receives only that branch's history, gets a revision per push, and merges it like any other.
- A merge request from a less visible fork needs the author's confirmation that its commits become readable in the original; the timeline records it.
- Pushes to a fork's merge request branch meet the original's push protection before the original takes them over; findings show on the merge request with allow links.
- Writers of the original can push to an open fork merge request's branch unless its author turns edits by maintainers off; each such push is audited.
- The compare page can merge from a fork, with Allow edits by maintainers and the confirmation for less visible forks; forks link to it from Contribute, pushes, and the banner.
- Fork merge requests show their source as owner/repository:branch, and the More menu turns edits by maintainers on or off.
- `#n` links to merge requests as well as issues, with their state, and issues and merge requests show where they were mentioned, once per source.
- `Fixes #n`, `Closes #n`, and `Resolves #n` close the issue when the commit lands on the default branch or the merge request with it in its description is merged there.
- Closing keywords act only on issues the pusher or merger may close; the issue's timeline links the commit or merge request that closed it.
- Update branch merges the target into a merge request's branch or rebases the branch onto it on the server, keeping change IDs; the result is a new revision.
- Writers of the original can update a fork merge request's branch while edits by maintainers are allowed; the source branch's protection applies as for a push.
- Merge when ready merges a merge request as the person who set it as soon as every requirement is met, once; a push by anyone else cancels it.
- Whoever sets merge when ready is notified when it merged, failed, or was cancelled; the timeline records each step.
- The merge box offers Merge when ready while a requirement is missing, and once it's set, shows who set it with a button to cancel.

### Changed

- Updating means installing the new version and restarting: Slath checks, backs up, and migrates when it starts, and refuses to start, changing nothing, when it can't update safely.
- Pre-upgrade backups are on by default everywhere, and only the newest two are kept (`[upgrade] backups_kept`).
- `slath serve` has no `--migrate` flag any more; it always applies a pending update first.
- Docker Compose runs the published image and needs only the database password and the public URL in `.env`; backups get their own volume.
- The Compose TLS profile serves the host in `SLATH_PUBLIC_URL` and trusts only its own proxy address, without a subnet to look up.
- The first administrator is created with one `docker compose run` command.
- Access tokens have a new format: a prefix that names the kind, and a checksum so mistyped or fake tokens are rejected at once. Tokens from 0.1 and 0.2 no longer work; create new ones.
- The top bar and favicon show the new Slath logo, in the variant for the light or dark theme in use.
- Repository pages: Clone and Download are menus next to the branch, clone URLs have copy buttons, and the files sit in a rounded box.
- The branch name above the files opens a branch picker that keeps the current view and path.
- A fork's origin shows next to the repository name, and how its default branch compares with the original next to the branch.
- A side menu on the left shows where you are (a repository, an organization, your settings, administration, or your work) and its areas.
- The side menu collapses with the button at the start of the top bar, and the choice is remembered; on narrow screens it opens as a drawer.
- Repository pages: Code, Issues, Merge requests, and Settings are in the side menu; Files, Commits, Branches, Tags, and Forks are tabs under Code.
- Issues, Labels, and Milestones, the repository settings sections, and a merge request's Conversation, Commits, and Changes are tabs.
- Open, Closed, and All on issue and merge request lists, All and Unread in notifications, and All, Unresolved, and Mine in a merge request are a menu.
- Merge request checkout commands are in a Check out menu next to the title, with icon copy buttons.
- The repository description shows under the repository name on every repository page, above the tabs.
- Fork is offered only where you can fork to: not into the original's own owner, so your own repositories need an organization to fork into.
- A README's file name sits above its box, so it doesn't read as part of the file.
- The Clone and Check out menus say what they're for, and checkout commands sit in bordered fields to copy, like clone URLs.
- The top bar spans the window: the logo lines up with the side menu's entries, and the icons on the right keep the same distance from the edge.
- Moving between pages no longer reloads the whole window: links swap the page next to the top bar, and every address can still be reloaded and shared.
- Choosing a list view such as Closed, filtering, or paging replaces only the list.

### Removed

- Databases and backups from the 0.1 and 0.2 previews can no longer be upgraded or restored. Install 0.3 with an empty database and import the repositories; see the upgrading guide.

### Fixed

- Two merges of the same merge request at once can no longer both land; the second one stops with a conflict.
- Labels of choices with a bold name, such as visibility and push protection, wrap as one sentence instead of splitting into columns.
- The jj checkout command for a merge request from a fork fetches the branch from the fork.
- The administration Upgrades page has a proper browser tab title.
- Re-running `slath upgrade` right after an interrupted upgrade no longer fails because the new backup would reuse the earlier backup's directory.

## [0.2.0] - 2026-11-02

Team alpha: issues, notifications, single sign-on, and one-command upgrades. Administrators need two-factor authentication; see `docs/admin/upgrading.md` before upgrading.

### Changed

- Administrators need two-factor authentication to use administration unless `require_mfa_for_admins` is turned off; see the upgrading guide.
- Search, notifications, settings, administration, sign out, and edit or delete actions are icon buttons with names for screen readers and tooltips.
- The Slath logo appears in the top bar and as the favicon, in the variant for the light or dark theme in use.
- The top bar shows your avatar instead of your user name; it links to your profile and shows the name as a tooltip.
- Repository pages show Watch/Unwatch on every tab, at the right of the repository name on wide screens.
- Issues, Labels, and Milestones are a side menu, like the repository settings; section headings have more space above them.

### Added

- Secrets that Slath must read again are encrypted with AES-256-GCM under the master key; each value is bound to its row.
- `slath admin rotate-key` re-encrypts stored secrets with a new master key and retires old keys once nothing uses them.
- `slath config migrate` rewrites renamed configuration keys and keeps comments; old keys keep working with a warning.
- `slath upgrade` upgrades from any earlier release in one command: preflight checks, an automatic backup, migrations, verification, and a history.
- `slath upgrade --check` shows the plan and the upgrade notes of every skipped release without changing anything.
- During an upgrade a running server becomes read-only: pages show a banner, and saving and pushing are refused with a message.
- `serve --migrate` runs the same preflight and refuses to migrate when a check fails or a manual upgrade step is required.
- An older binary on an upgraded database names the upgrade and the backup to restore.
- Issues: open, edit, close with a reason, reopen, and comment, with a timeline of changes, in the web UI and the API.
- References in issues and comments (`#12`, `owner/repo#12`, `@name`, commit IDs) link only for people who can read the target.
- Reference labels (`~bug`, `~"needs review"`), milestones (`%v1.0`), and teams (`@org/team`) in issues and comments.
- Reference suggestions for the editor and the API; they cover only readable repositories and never show email addresses.
- Labels and milestones for issues, managed by maintainers in the web UI and the API.
- Assign up to 10 people to an issue, set its labels and milestone; each change is shown on the timeline.
- Filter issues by labels, assignee, author, and milestone, and sort them by last update.
- A Markdown editor for issues and comments with rich-text and source modes, a toolbar, shortcuts, and a server-rendered preview.
- The editor suggests people, issues, labels, and milestones, uploads pasted or dropped files, and keeps unsent drafts in the browser.
- Internal repositories: visible to every signed-in user and "not found" for signed-out visitors and anonymous Git clients.
- Sign in with OpenID Connect providers (`[[auth.oidc]]`), link them under Settings → Security, and optionally register at first sign-in.
- OpenID sign-ins check every token property, never link accounts by email, and still ask for the second factor.
- Import a repository (all branches and tags) from an https:// URL in the web UI or the API, with a report of what arrived.
- Imports refuse internal and reserved addresses, pin the checked address, ignore redirects, and don't keep credentials.
- Blame view: see which commit last changed each line of a file, linked from every file page.
- Search issues (titles, descriptions, comments) and repositories, in the web UI and the API, showing only what you can read.
- Notifications about issues you take part in, are mentioned or assigned in, or watch, in an inbox and by email.
- Watch or unwatch repositories and subscribe to or unsubscribe from single issues; emails have a one-click unsubscribe link.
- New repositories start with a set of common labels such as bug, enhancement, and good first issue.
- Comment and close (or reopen) an issue in one step, also with Ctrl+Shift+Enter; the assignee field suggests names as you type.
- Issues show when they were last updated.
- Notifications stop, and older ones are hidden, when someone loses access to the repository.
- Attach files to issues and comments (web UI and API); images display inline, everything else including SVG downloads.
- Attachments of private repositories are only served to people who can read the repository.
- Upload avatars for users and organizations; they are re-encoded as 256 × 256 PNG without EXIF or GPS metadata.
- Unused attachment and avatar files are deleted after a grace period (`limits.blob_grace_period`); backups include them.
- Branch protection: block force pushes and deletion, limit who may push directly, and require merge requests with an audited admin bypass.
- Teams: organization owners group members into teams, and repository administrators give teams a role.
- Organizations can require two-factor authentication for access to their repositories and give every member read access.
- Registration modes: `disabled` (default), `invite` with single-use invitation links from administrators, and `open` self-registration.
- On open instances with email, accounts must confirm their address before creating repositories or organizations.
- Password reset by email: single-use links valid for 30 minutes, the same answer for unknown addresses, and every session signed out afterwards.
- Email addresses are confirmed by a link; users change theirs under Settings → Profile, and resets go only to confirmed addresses.
- Two-factor authentication with authenticator apps (TOTP) and single-use recovery codes, set up under Settings → Security.
- Passkeys: sign in without a password, use one as the second step, or confirm sensitive actions with it; managed under Settings → Security.
- Signing in and confirming sensitive actions ask for a two-factor code when it's set up; five wrong codes end the attempt.
- With `require_mfa_for_admins` (on by default), administration works only after signing in with a second factor.
- `slath admin reset-mfa` removes a user's second factors after a lost device; audited, and the user is notified by email.
- Outgoing email over SMTP with retries; `slath admin send-test-email` and the admin Health page check the settings.
- Without SMTP settings, email features are hidden.
- `slath serve` waits up to a minute for the database at startup instead of failing at once.
- Long data migrations run in the background in resumable batches; `slath admin background-migrations` and the admin page Upgrades show progress and pause, resume, or retry them.
- Every release ships machine-readable upgrade notes, and releases without them fail the release checks.

### Fixed

- The Commits tab of a repository without commits, or whose default branch doesn't exist yet, now explains that instead of showing "Not found".

## [0.1.1] - 2026-11-01

Security hardening: the SSH login no longer shares the server's system user. See `docs/admin/upgrading.md` before upgrading a systemd installation.

### Security

- The SSH login `git` can be a separate system user that reaches only the server's socket; Git for SSH now runs in the server, so the login can't read repositories or secrets.
- The container image and the Compose setup separate the SSH login by default, and the sshd container no longer mounts the data volume.
- `slath config check` fails if the SSH login's group can read the data directory, the master key, or the database URL file.

### Added

- `storage.socket_group` lets the SSH login's group connect to the server's socket. Without it, only the server's own user can, as before.

### Changed

- The sshd drop-in runs `slath ssh-keys` as `git` instead of `slath`, and the systemd files create `git` as its own user. See `docs/admin/upgrading.md` for existing installations.
- The install guide shows how to trust only the reverse proxy's actual subnet instead of a broad private range.

## [0.1.0] - 2026-11-01

First release: self-hosted Git and Jujutsu hosting for small teams.

### Added

- Cargo workspace with the `core`, `db`, `git`, `markup`, `auth`, `service`, `http`, and `ci` crates, the `slath` binary, strict workspace lints, and `just check`.
- License texts under `LICENSES/`, a `cargo deny` policy, and a check that no Apache-2.0 crate depends on an AGPL crate.
- Core types: UUIDv7 IDs, name validation with reserved names, the redacting `Secret<T>`, and authorization types.
- Hardened Git runner: scrubbed environment, fixed config, timeouts, output caps, concurrency limit, and a check for the newest stable Git.
- Validators for revisions, ref names, and paths; an object reader over `git cat-file`; a byte-exact commit parser that exposes jj change IDs.
- CI running `just check`, an MSRV build, and `scripts/guards.sh`, which enforces SPDX headers, no shells, no `|safe`, and no third-party URLs.
- Developer environment: PostgreSQL 18 via `deploy/dev/compose.yml`, `just setup`, `just dev-db`, and `docs/dev/setup.md`.
- Database layer: connection pool, embedded migrations with a schema version check, and the first schema.
- Durable job queue with dedupe keys, `SKIP LOCKED` leases, heartbeats, exponential backoff, a dead state, and `LISTEN/NOTIFY` wake-ups.
- Deterministic Git fixture generators with identical OIDs on every run, and installers for SHA-256-pinned Git and jj versions.
- Conformance harness: JUnit output and `just conformance-report`, which maps `conf_*` tests to conformance cases.
- Git smart HTTP (protocol v0 and v2) with managed pre- and post-receive hooks that talk to the server over a 0600 Unix socket and fail closed.
- Git over SSH through OpenSSH: `slath ssh-keys` and `slath ssh-serve` allow only fetch and push on permitted repositories; shells and forwarding are refused.
- `deploy/sshd/slath.conf`, the sshd drop-in, tested against a containerized sshd.
- Jujutsu compatibility tests: `jj git clone` and `jj git push` over HTTPS and SSH for every pinned jj version.
- README with the project's purpose, status, and development quick start.
- TOML configuration with `SLATH__<SECTION>__<KEY>` overrides, typed defaults, all limits under `[limits]`, and errors naming unknown keys.
- `slath config check`, `slath version`, `slath migrate`, and `slath admin gen-key`; admin commands accept `--output json`.
- HTTP server basics: request IDs, access logs, Prometheus metrics on a separate listener, body limits, and security headers with a CSP nonce.
- RFC 9457 problem JSON for API errors, HTML error pages for the UI, and `/-/healthz`, `/-/readyz`, and `/-/csp-report`.
- UI shell: Askama layouts with landmarks and skip link, and a Vite bundle (htmx 4 with `hx-csp`) embedded under content-hashed URLs.
- Development-only component catalog at `/-/design`.
- Browser tests (`just test-e2e`) in Chromium, Firefox, and WebKit, light and dark, desktop and touch, with axe accessibility checks.
- Weave design tokens: light, dark, and high-contrast colors plus typography, spacing, radii, motion, and density, with contrast checks.
- Bundled Source Sans 3 and JetBrains Mono fonts (OFL-1.1) with metric-matched fallbacks, and a Lucide (ISC) icon sprite.
- `THIRD-PARTY-NOTICES.md` lists every bundled component; `just check` fails if one is missing.
- Local accounts: `slath admin create-user`, Argon2id password hashes, and a 12–1024 character password policy.
- Sign-in and sign-out with hashed session cookies, idle and absolute expiry, and throttling per account and per IP.
- CSRF protection for every state-changing browser request: same-origin checks plus a token.
- Account settings: display name, generated avatar, password change, and a sessions list with sign-out per session or everywhere.
- Sudo mode: sensitive actions ask for the password again, valid for 15 minutes.
- Personal access tokens with scopes and expiry, shown once and stored only as a hash.
- `/api/v1` accepts only `Authorization: Bearer` tokens and ignores cookies.
- SSH keys in account settings: Ed25519, ECDSA, security keys, and RSA of at least 3072 bits; weak or malformed keys are rejected with a reason.
- Organizations with owners and members, a protected last owner, and profile pages at `/<name>`.
- Users and organizations share one case-insensitive namespace.
- Repositories: create in your namespace or an organization, with clone URLs and push instructions on the empty-repository page.
- Repository settings: description, default branch, visibility, rename with redirects for readers, archive, and collaborators with roles.
- Repository deletion: invisible at once, then kept in the trash for `maintenance.deleted_repo_days` before it's purged.
- Authorization policy for every repository action, pinned by a 1444-cell matrix test; invisible resources answer "not found".
- Git over HTTPS and SSH serve repositories from the database; HTTPS uses tokens as passwords, and private and missing repos look the same.
- Push policy: internal refs are refused, pushes need write access, archived repos are read-only, and moving tags needs the maintain role.
- Accepted pushes are recorded as push events, ref updates, and a ref snapshot.
- Reconciler: `slath fsck --reconcile` and `slath serve` repair database refs that differ from Git.
- Markdown rendering with an allowlist sanitizer, CSS-class syntax highlighting, and visible text-direction controls; pinned by an XSS corpus.
- Code browser: files, README, highlighted code with line anchors, history, commits with diffs, and branch and tag lists.
- Raw files are served as text, verified images, or downloads, always with a sandbox CSP; symlinks are served as their target text.
- Archive downloads as `.tar.gz` and `.zip`, with the code browser's access rules, a timeout, a size cap, and a concurrency limit.
- REST API for the user, keys, tokens, organizations, members, repositories, settings, collaborators, refs, history, and file contents.
- API cursor pagination with `Link` headers, rate limits with `RateLimit-*` headers, and `Idempotency-Key` on mutating requests.
- OpenAPI document at `/api/v1/openapi.json`, committed as `docs/api/openapi.json`; contract tests check real responses against it.
- Append-only audit log of sign-ins, passwords, sudo, tokens, keys, organizations, and repositories, with no secrets; old entries are purged.
- Site administration at `/-/admin`: create, suspend, and promote users, restore deleted repositories, and view the audit log and health.
- Suspending a user signs them out everywhere and revokes their tokens.
- Organization owners see their organization's audit log at `/<org>/-/audit`.
- `slath backup create` takes a consistent backup while the server runs, without `pg_dump`, with a checksummed manifest.
- `slath backup restore` verifies the whole backup first, never takes repository config from it, and reconciles afterwards.
- `docs/admin/backup.md` and `just test-backup-restore`, the restore drill.
- Release container image with the embedded UI, a pinned Git, and OpenSSH, running as an unprivileged user.
- `deploy/compose/` (PostgreSQL, Slath, sshd, optional Caddy TLS), `deploy/systemd/`, `deploy/slath.example.toml`, and `docs/admin/install.md`.
- `slath admin gen-key --output FILE` writes a new master key with mode 0600 and never replaces an existing one.
- `just test-compose` follows the install guide end to end and renders every page of the image with outbound networking disabled.
- Repository maintenance runs `git gc` after 50 pushes or daily, never twice at once on a repository, and records the repository size.
- Pushes warn when a repository exceeds `limits.repo_soft_quota_bytes`.
- Every HTTP route declares its access level; Clippy rejects routes without one.
- Security tests sweep every route as anonymous and as a stranger, and check the authorization matrix on every surface, including SSH.
- A log test checks that passwords, tokens, cookies, and keys never reach the log, even at debug level.
- Regression tests modeled on recent advisories in other forges and in Git, covering injection, traversal, authorization, XSS, and CSRF.
- Appearance menu in the header for theme and density, saved in cookies or the account, and rendered by the server for the first paint.
- Settings → Appearance sets theme, density, and code font size; touch screens get 44 px targets in every density.
- Weave components: menus, dialogs, drawers, toasts, and loading and error states, shown on the component catalog.
- `just css-lint` rejects raw colors, font sizes, spacing, and shadows outside the design tokens.

### Fixed

- API tokens without `user:read` could list the user's organizations and their members.
- API tokens without `repo:read` could list the user's private repositories.

[Unreleased]: https://github.com/slath-org/slath/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/slath-org/slath/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/slath-org/slath/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/slath-org/slath/releases/tag/v0.1.0
