<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Bots

A bot is an account for automation: a CI system, a release script, or an
AI coding agent. It belongs to a person or an organization, acts only
through the credentials its owner gives it, and is marked as a bot
wherever it appears, so nobody mistakes its work for a person's.

## Creating a bot

Anyone can create bots of their own under **Settings → Bots**. An
organization's bots are created by the organization's owners on the
organization's **Bots** page. Each bot has a page (`/-/bots/<name>`) for its
tokens and SSH keys, which only its managers can open: its owner, or for
an organization's bot, the organization's owners.

Through the API, with a token that has the `user:write` scope:

```sh
# A bot of your own
curl -X POST https://git.example.org/api/v1/user/bots \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"username": "alice-ci", "display_name": "Alice'\''s CI"}'

# A bot of the organization acme (organization owners)
curl -X POST https://git.example.org/api/v1/orgs/acme/bots \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"username": "acme-release"}'
```

A bot's name follows the same rules as usernames and shares their
namespace: it can't be the name of a user, an organization, or another
bot. `GET /api/v1/user/bots`, `GET /api/v1/orgs/{org}/bots`, and
`GET /api/v1/bots/{name}` list and show the bots you manage. Other
people's bots don't appear there.

A bot's page also changes its avatar (PNG, JPEG, or WebP, cropped to a
square and stored without metadata) and its display name. Without an
uploaded avatar, one is generated from the bot's name.

## Tokens

A bot works through tokens that start with `slath_bot_`. They are like
personal access tokens, with stricter rules:

- **Restricted to repositories.** Each token names 1 to 50 repositories,
  and works for nothing else. Name only repositories the bot needs.
- **Limited scopes.** `repo:read`, `repo:write`, `issues:write`,
  `mrs:write`, `statuses:write`, and `user:read`. Never `user:write` or
  `admin`: a bot can't change its own keys or tokens.
- **Short-lived.** A bot token expires after 30 days unless its owner
  picks another period, and after 90 days at the most. Site
  administrators can change both (see
  [Accounts](../admin/accounts.md#bots)).

The token is shown once, when it's created. Create one under **Tokens** on
the bot's page; Slath asks for your password first if you haven't
confirmed it recently. A site administrator can also create one on the
server:

```sh
slath admin create-bot --owner alice --name alice-ci \
  --token-scopes repo:write,mrs:write,statuses:write --repo alice/app
```

`slath admin create-bot` creates the bot, and with `--token-scopes` also a
token for the repositories given with `--repo` (repeat it for several);
`--token-days` sets the validity and `--output json` prints the result as
JSON. Tokens can't be created through the API, just as personal tokens
can't.

`GET /api/v1/bots/{name}/tokens` lists a bot's tokens (never the
secrets), and `DELETE /api/v1/bots/{name}/tokens/{id}` revokes one. A
`slath_bot_` token only ever authenticates a bot, and a `slath_pat_` token
only a person.

A bot still needs a role on each repository, like anyone else: add it as
a collaborator or to a team. The token's repositories only narrow what
that role allows.

## SSH keys

For Git over SSH, give the bot its own key pair. Its owner adds the public
key under **SSH keys** on the bot's page, or through the API with a token
that has `user:write`:

```sh
curl -X POST https://git.example.org/api/v1/bots/alice-ci/keys \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d "{\"title\": \"ci runner\", \"key\": \"$(cat ci.pub)\"}"
```

`GET /api/v1/bots/{name}/keys` lists them and
`DELETE /api/v1/bots/{name}/keys/{id}` removes one.

## Roles and their limits

A bot gets roles on repositories like anyone else: as a collaborator or
through a team. A bot that belongs to a person can never do more than
that person on the same repository: its role is the lower of its own and
its owner's. A bot with maintain access to a repository where its owner
only reads can only read; a repository its owner can't see doesn't exist
for the bot either, in lists and search included. An organization's bot
isn't limited this way, since organization owners manage it.

The owner of a bot also owns what's in the bot's namespace, such as forks
the bot made; for an organization's bot, the organization's owners do.

When a site administrator suspends a person, their bots are suspended
too, and they come back when the person is reactivated. A bot that was
suspended on its own stays suspended, and a bot can't be reactivated
while its owner is suspended.

## Guardrails on protected branches

Branch protection (repository **Settings → Branch protection**, or
`POST /api/v1/repos/{ns}/{repo}/protections`) keeps bots in check:

- **Approvals by bots don't count** toward the approvals a protected branch
  needs, unless the rule has **Count approvals by bots** on
  (`bots_count_as_approvers`). Even then, a bot never approves its own
  owner's merge request, just as its statuses never count for its owner's
  required checks.
- **Bots don't merge into or push to protected branches**, whatever their
  role, unless the rule lists them under **Bots that may merge and push**
  (`bot_merge_allowlist`). This covers merging by hand, merging when ready,
  updating a merge request's branch, and pushing directly. When several
  rules match a branch, every one of them must list the bot.

## Rate limits

Bots make at most 2,000 API requests per hour (site setting
`[limits] bot_requests_per_hour`), fewer than people. The response headers
`RateLimit-Limit`, `RateLimit-Remaining`, and `RateLimit-Reset` say where a
bot stands.

## How bots appear

A **Bot** badge follows a bot's name wherever Slath shows it: as the author
of issues, merge requests, and comments, in reviews, review threads, the
timeline, commit statuses, member and collaborator lists, and the audit
log. A bot's profile says who owns it. Comments by bots are also marked as
claims: Slath shows what a bot says, not that it's true.

## What a bot can't do

- **Sign in.** A bot has no password, no session, no second factor, and
  no OpenID Connect link. The sign-in page refuses its name exactly as it
  refuses an unknown one.
- **Own organizations or create bots.** Bots can be members of an
  organization but never its owners, and they can't create organizations
  or bots of their own.
