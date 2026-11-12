<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Push protection

Slath rejects pushes that contain secrets before anything of them becomes
visible: no branch moves, no objects stay in the repository, and nothing
triggers notifications, merge request updates, or webhooks. It works the
same for Git and jj over HTTPS and SSH, and needs no client setup.

What it promises: every supported way into a repository enforces the
repository's configured policy before new content becomes visible. It
doesn't promise that no secret can ever enter: the `warn` and `off` modes,
allowances, the allowlist, and report-only imports are deliberate
exceptions, and the rules find known formats, not every secret.

A matched secret never appears in full anywhere: not in the push output,
API errors, logs, the audit log, notifications, or webhooks. Each rejected
push is recorded in the audit log as `push.secret_rejected`, with the rule
IDs and fingerprints but no secret, file name, or excerpt.

## What is scanned

Only what a push adds to the repository: the files of the new commits
(binary files are skipped), the new commit messages, and new annotated tag
messages. Content the repository already has is never scanned again.

Merge requests from forks are scanned with the target repository's
settings, whatever the fork's own mode is. Before Slath takes over a new
push to the fork's branch, it checks what that push would add to the
target, and nothing is copied until the check passes. A blocking finding
holds the push back: the merge request keeps its previous revision and
lists the findings (redacted), with allow links for the target's writers.
An allowance in the target, or a new push without the secret, brings the
merge request up to date. The target's allowlist applies as for a push.

Repositories imported from another server are scanned after the import,
but only reported: the history exists elsewhere already. The repository's
front page tells its administrators how many possible secrets were found,
so they can revoke them. Findings the imported default branch's allowlist
covers are counted apart, as allowed by the repository.

Merge and squash messages and applied suggestions are scanned too, with
the repository's allowances and allowlist, as for a push.

## What a rejection looks like

```text
remote: slath: push rejected: 1 secret(s) found (push protection)
remote: slath:   commit 3f2a9c81d0e4 config/deploy.env line 4: GitHub token (github-token)
remote: slath:     GITHUB_TOKEN=ghp_…
remote: slath:   Remove the secret from every commit that has it, revoke it, then push again:
remote: slath:     Git: git rebase -i 3f2a9c81d0e4^ (mark the commit "edit", fix it, git commit --amend, git rebase --continue)
remote: slath:     jj:  jj edit 3f2a9c81d0e4 (fix the file; descendants are rebased), then jj git push
remote: slath:   Not a secret? A repository writer can allow it, then push again unchanged:
remote: slath:     https://git.example.org/team/app/-/secret-scan/allow?fingerprint=9b1f…&rule=github-token
```

The secret itself is never printed in full. Revoke it even after removing
it: it left the machine it was on.

## False positives and fixtures

- **One finding:** a writer of the repository opens the link from the
  rejection and allows that finding for 1, 7, or 30 days, with an optional
  reason. The allowance covers that one value (by its fingerprint) in this
  repository only. Then push again; no push options are needed, so it
  works the same with jj. The page lists the allowances in force, and each
  one is recorded in the audit log (`secret_scan.allow`). Bots can't allow
  findings. Where policy wants a second person to decide, check **Only
  administrators may allow findings** next to the mode: then writers get
  the link but can't use it.
- **Lasting exceptions:** add `.slath/secret-scan.toml` to the default
  branch:

  ```toml
  [[allow]]
  path = "tests/fixtures/**"          # `**` any path, `*` within a segment, `?` one character
  reason = "test keys, never used anywhere"

  [[allow]]
  fingerprint = "9b1f0c3e5a7d2b4f6e8a0c2d4f6b8a0e"   # from a rejection link
  ```

  It counts only as it stands on the default branch, so a push can't
  allow itself: an entry added together with the secret has no effect.
  Merge it first, for example in a merge request of its own.
- **Moving an existing repository here:** the first push into an empty
  repository may contain secrets that are exposed elsewhere already. Its
  rejection then also prints a link for repository administrators, who can
  accept all of that push's findings in one audited step
  (`secret_scan.accept_first_push`); they're allowed for a day. Revoke
  those secrets anyway.

## Repositories with secrets on purpose

Some repositories hold credential-shaped content deliberately: test keys
and certificates, scanner rule tests, security training material,
honeytokens. Slath never changes or hides repository content in its
views, diffs, raw files, archives, forks, or backups; push protection
only decides what gets in. To keep such a repository usable:

1. **Allowlist first.** Put the fixtures' paths in `.slath/secret-scan.toml`
   on the default branch (see above), and merge that file before the
   fixtures. From then on, pushes, merge and squash messages, applied
   suggestions, and merge requests from forks all pass for those paths
   and fingerprints, without any further step.
2. **Or allow a whole push.** When a push with two or more blocking
   findings is rejected, the output also prints one link for all of them:

   ```text
   remote: slath:   Or allow all 10 findings of this push at once, with one reason and one period:
   remote: slath:     https://git.example.org/team/app/-/secret-scan/accept/0190…
   ```

   Whoever may allow findings (writers, or administrators when **Only
   administrators may allow findings** is checked; with sudo, never a bot
   or a token) allows them all with one reason for 1, 7, or 30 days. One
   audit event, `secret_scan.allow_push`, lists every fingerprint. The
   link stays valid for 24 hours. Each finding's own allow link leads to
   it as well. Both pages say what each finding is (for example "OpenSSH
   private key") and where it is (file, line, and commit), never the
   secret itself.
3. **Forks** copy the history without scanning it again, and the
   allowlist comes along with the default branch.
4. **Imports** count findings covered by the imported default branch's
   allowlist apart: the report's `secret_findings_allowed`, and on the
   front page as allowed by the repository, not as secrets to revoke.

Where the site sets `minimum = "block"`, the mode `off` isn't available;
path entries in the allowlist are the way to hold fixtures there.

## Modes

Each repository has a mode under **Settings → General → Push protection**
(administrators of the repository, with sudo):

- **block** (the default): block-rule findings reject the push; warn-rule
  findings are printed as warnings.
- **warn**: every finding is printed as a warning; nothing is rejected.
- **off**: pushes aren't scanned.

The site configuration can enforce a minimum for every repository:

```toml
[push_protection]
minimum = "block"   # off (default) | warn | block
```

## Limits

The scan is bounded and fails closed: a push whose scan can't finish is
rejected in `block` mode, with the limit named, and an administrator can
raise the limit.

```toml
[limits]
secret_scan_file_max_bytes = "8MiB"    # larger text files can't be scanned
secret_scan_push_max_bytes = "512MiB"  # all new text of one push
secret_scan_timeout = "60s"
```

## Rules

Rule set version `2026.11.1`. Matching runs in time linear in the text.
Placeholder values (`example`, `changeme`, `<token>`, `${VAR}`, repeated
characters, and similar) are ignored.

| Rule | Tier | Finds |
|---|---|---|
| `slath-token` | block | Slath's own tokens (`slath_pat_`, `slath_bot_`, `slath_tsk_`, `slath_rnr_`), exactly, by prefix and checksum |
| `private-key` | block | PEM private key blocks with key material (RSA, DSA, EC, OpenSSH, PGP, PKCS #8); findings name the key type, such as "OpenSSH private key" |
| `github-token` | block | GitHub tokens (`ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`) |
| `github-fine-grained-token` | block | GitHub fine-grained tokens (`github_pat_`) |
| `gitlab-token` | block | GitLab personal access tokens (`glpat-`) |
| `aws-access-key-id` | block | AWS access key IDs (`AKIA…`, `ASIA…`) |
| `slack-token` | block | Slack tokens (`xoxb-`, `xoxp-`, …) |
| `stripe-secret-key` | block | Stripe live secret and restricted keys (`sk_live_`, `rk_live_`) |
| `google-api-key` | block | Google API keys (`AIza…`) |
| `npm-token` | block | npm access tokens (`npm_`) |
| `pypi-token` | block | PyPI API tokens (`pypi-AgEIcHlwaS5vcmc…`) |
| `url-credentials` | block | Passwords in URLs and connection strings (`scheme://user:password@host`) |
| `generic-assignment` | warn | Assignments such as `password = "…"`, `api_key: "…"` |

Turn off built-in rules, or add your own:

```toml
[push_protection]
disable = ["url-credentials"]

[[push_protection.rule]]
id = "acme-key"
description = "ACME internal key"
regex = 'acme_(?P<secret>[0-9a-f]{32})'   # the "secret" group, if present, is the secret
tier = "block"                             # block | warn
```

Teams can run the same patterns locally (for example in an editor or a
pre-commit tool); nothing requires it.
