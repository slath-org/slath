<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Working with AI agents

Slath runs no agents and no models. It gives the agents you run (a coding
assistant, a CI job that fixes lint, a dependency updater) an identity of
their own, credentials that can do only what they need, and surfaces that
are easy for programs to read and safe to retry. People review and merge;
agents propose.

Everything here works the same whether the agent uses Git or jj.

## Try it in a sandbox

`deploy/compose/agent-sandbox/` starts a throwaway Slath on your computer
in one command. It's **not for production**: the database password is
fixed, there's no TLS, and administrators need no second factor.

```sh
cd deploy/compose/agent-sandbox
docker compose up -d          # Podman: podman compose up -d
docker compose logs seed      # wait for "Slath agent sandbox"
```

It serves <http://127.0.0.1:3080> (SSH on port 2280) and starts with:

| What | Name | Credential |
| --- | --- | --- |
| An administrator | `ada` | password in `secrets/ada-password` |
| An organization | `sandbox` | |
| A repository | `sandbox/playground` | |
| A bot with write access to it | `agent` | token in `secrets/bot-token` |

Point your agent at it:

```sh
export SLATH_TOKEN=$(cat secrets/bot-token)
git clone "http://agent:$SLATH_TOKEN@127.0.0.1:3080/sandbox/playground.git"
curl -H "Authorization: Bearer $SLATH_TOKEN" http://127.0.0.1:3080/api/v1/user
```

To add accounts, bots, or repositories, edit `seed.toml` (the format is in
[Setting up an instance from a file](../admin/bootstrap.md)) and run
`docker compose up -d` again; nothing that exists is changed. The ports
can be changed with `SANDBOX_HTTP_PORT` and `SANDBOX_SSH_PORT`.
`docker compose down -v` and deleting `secrets/*` start over.

## One bot per agent

Give every agent its own [bot](bots.md) instead of a person's token:

- Its work is marked as a bot's everywhere, and people can tell which
  agent did what.
- A person's bot can never do more than that person, and an
  organization's bot only what its owners grant it.
- Its tokens are restricted to repositories and expire after 30 days by
  default, so a leaked token has a small reach.
- Stopping one agent means stopping one bot, which revokes all its
  credentials at once (see [Stopping a bot](bots.md#stopping-a-bot)),
  without touching anyone else's access.

For a set of agents that a team runs, create the bots in the team's
organization; for your own assistant, a bot of your own is enough.

## Least privilege

Pick the narrowest role and scopes that let the agent do its job:

| The agent… | Role | Token scopes |
| --- | --- | --- |
| reads code and answers questions | read | `repo:read` |
| comments on issues and merge requests | read | `repo:read`, `issues:write`, `mrs:write` |
| reports checks and findings | write | `repo:read`, `statuses:write` |
| pushes branches and opens merge requests | write | `repo:write`, `mrs:write` |

Protect the branches that matter (**Settings → Branch protection**): bots
can't push to or merge into a protected branch unless its rule lists them,
and their approvals don't count unless the rule allows it. A bot's
approval never counts for its own owner's merge request, and its statuses
never count for its owner's required checks.

## Branches or forks

- **Branch-based:** the bot has the write role and pushes to its own
  branches (for example `agent/<topic>`), then opens a merge request. Keep
  the default branch protected so only people merge.
- **Fork-based:** the bot has the read role on the repository and works
  in its own fork (`POST /api/v1/repos/<ns>/<repo>/forks`). Its merge
  requests come from the fork, and the original's maintainers decide what
  lands. Since a bot token works only for the repositories it names, the
  bot pushes to its fork with an SSH key of its own, or with a token its
  owner creates for the fork afterwards (the owner of a bot can manage the
  bot's repositories). Use this for agents you trust less, or for
  repositories the agent shouldn't push to at all.

## Surfaces for programs

- **The API** (`/api/v1`, described in `GET /api/v1/openapi.json`)
  authenticates with `Authorization: Bearer <token>`. Errors are RFC 9457
  documents with a stable `code`.
- **Safe retries:** send an `Idempotency-Key` header with any request that
  changes something. Repeating it within 24 hours returns the first
  response instead of doing it twice.
- **Plain text:** a commit's or merge request's diff and patches, ready for
  `git apply` and `git am`, at
  `GET /api/v1/repos/<ns>/<repo>/merge-requests/<n>/diff` and `/patch`
  (and `…/commits/<oid>/diff` and `/patch`).
- **What blocks a merge:** `GET …/merge-requests/<n>/mergeability` lists
  every requirement with its state and reason.
- **Findings:** a reviewing agent or linter reports a status with line
  annotations; they show as threads on the merge request revision whose
  head it annotated, and resolve when a later status no longer reports
  them (see [findings in the review](merge-requests.md#findings-in-the-review)).
- **Events:** [webhooks](webhooks.md) tell an agent about pushes, comments,
  reviews, and statuses, so it doesn't have to poll.
- **Rate limits:** bots get 2,000 requests per hour by default; the
  `RateLimit-*` response headers say how many are left.

## What Slath doesn't do for you

Slath treats what an agent writes as its claim: comments by bots are
marked as such, and a bot's status says what it reported, not that it's
true. Review agent changes like any other contribution; Slath makes sure
that someone has to.
