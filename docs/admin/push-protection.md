<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Push protection

Slath rejects pushes that contain secrets before anything of them becomes
visible: no branch moves, no objects stay in the repository, and nothing
triggers notifications, merge request updates, or webhooks. It works the
same for Git and jj over HTTPS and SSH, and needs no client setup.

## What is scanned

Only what a push adds to the repository: the files of the new commits
(binary files are skipped), the new commit messages, and new annotated tag
messages. Content the repository already has is never scanned again.

Repositories imported from another server are scanned after the import,
but only reported: the history exists elsewhere already. The repository's
front page tells its administrators how many possible secrets were found,
so they can revoke them.

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
  findings.
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
| `private-key` | block | PEM private key blocks with key material (RSA, DSA, EC, OpenSSH, PGP, PKCS #8) |
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
