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
| pushes changes and opens their merge requests only by pushing | write | `repo:write` |

Any token that may push can open a merge request by pushing a branch
named like `push-…` or to `refs/for/<target>` (see
[Without naming a branch](merge-requests.md#without-naming-a-branch)). To
keep pushes from opening merge requests in a repository, empty
**Settings → General → Merge requests → Branches that open a merge
request**.

Protect the branches that matter (**Settings → Branch protection**): bots
can't push to or merge into a protected branch unless its rule lists them,
and their approvals don't count unless the rule allows it. A bot's
approval never counts for its own owner's merge request, and its statuses
never count for its owner's required checks.

Bots can't change CI workflows, code owners, Slath's `.slath/` files, or
agent instruction files such as `AGENTS.md` and `CLAUDE.md`: such pushes
are rejected (see [sensitive paths](bots.md#sensitive-paths)). Change
them yourself.

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

## Work orders: delegating one task

A work order hands one issue, or a follow-up on a merge request, to a
bot. You say what to do; the order keeps the bot to that task and records
what it did.

```sh
curl -H "Authorization: Bearer $SLATH_TOKEN" -H 'Content-Type: application/json' \
  https://slath.example.org/api/v1/repos/acme/app/work-orders -d '{
  "target": {"issue": 42},
  "bot": "acme-agent",
  "goal": "Make the parser accept trailing commas.",
  "instructions": "Keep the public API as it is.",
  "scope": ["src/parser/**"],
  "required_checks": ["ci/test"],
  "reviewers": ["alice"]
}'
```

- **Who:** anyone with write access delegates (a token with
  `repo:write`); bots can't. The bot needs the write role on the
  repository itself.
- **One at a time:** an issue or merge request has at most one open
  order. The order's number names the branches the bot works on,
  `wo/<number>/…`.
- **Limits:** `max_revisions` (default 20), `max_open_mrs` (default 1),
  and `expires_in_days` (default 7, at most 30). `scope` records which
  paths the work is meant for; pushes outside it aren't refused.
- **Text is checked:** goal and instructions are refused if they contain
  a secret, like briefs.
- **Reading and stopping:** everyone who can read the repository lists
  orders (`GET …/work-orders`) and reads one (`GET …/work-orders/<n>`).
  The creator, anyone with write access, and the bot's managers stop one
  with `POST …/work-orders/<n>/stop`.
- **Ending on its own:** an order ends `merged` when one of its merge
  requests merges and none of its others is open, `closed` when its
  issue (or a follow-up's merge request) closes or merges, `expired`
  when its time is up, and `stopped` when its bot is stopped or
  suspended or loses write access. Closing one of its own merge requests
  doesn't end it, so the bot can try again within the budget.
- **Events:** `GET …/work-orders/<n>/events` lists what happened under the
  order, kept as long as the repository.

The bot picks up its orders with a token that has only the
`work_orders:claim` scope. That token can't read code, issues, or merge
requests; it lists the bot's open orders (`GET /api/v1/user/work-orders`),
reads them, and claims one:

```sh
curl -X POST -H "Authorization: Bearer $CLAIM_TOKEN" \
  https://slath.example.org/api/v1/repos/acme/app/work-orders/3/claim
```

The answer holds a task token (`slath_tsk_…`), shown once, that lasts an
hour or until the order ends. Claim again before then to renew it; that
revokes the earlier one. Only the order's bot can claim it.

A task token acts with the bot's own role, but only for the order:

- It reads the repository through the API (issues and merge requests
  included), and nothing in other repositories, public ones included.
- It pushes branches under `wo/<number>/` over HTTPS, with the token as
  the password: plain `git push` or `jj git push`, nothing to set up. A
  push that touches any other branch or a tag is rejected as a whole. A
  push that would give the order's merge requests more than
  `max_revisions` revisions in all is rejected too. SSH needs the bot's
  own keys, not a task token.
- It comments on the order's issue (or, for a follow-up, its merge
  request) and on the merge requests it opened.
- It opens merge requests only from branches under `wo/<number>/`, at
  most `max_open_mrs` open at a time, and edits only those; it never
  merges.
- It reports statuses only on the heads of its merge requests, and those
  never count for their required checks, since the bot is their author.
- Anything else, such as repository settings or another issue, is
  refused. Every write it tries is an event of the order, with the rule
  that refused it.
- It stops working when the order ends, when it's renewed, and while a
  freeze covers the bot.

## Surfaces for programs

- **The API** (`/api/v1`, described in `GET /api/v1/openapi.json`)
  authenticates with `Authorization: Bearer <token>`. Errors are RFC 9457
  documents with a stable `code`.
- **Safe retries:** send an `Idempotency-Key` header with any request that
  changes something. Repeating it within 24 hours returns the first
  response instead of doing it twice. A refused request isn't kept: fix
  it and send it again with the same key.
- **Plain text:** a commit's or merge request's diff and patches, ready for
  `git apply` and `git am`, at
  `GET /api/v1/repos/<ns>/<repo>/merge-requests/<n>/diff` and `/patch`
  (and `…/commits/<oid>/diff` and `/patch`).
- **What blocks a merge:** `GET …/merge-requests/<n>/mergeability` lists
  every requirement with its state and reason.
- **One call per round:** `GET …/merge-requests/<n>/iteration?since=<revision>`
  returns the current revision, what changed since the revision you last
  worked on (the same text as `…/interdiff?from=<revision>`), the required
  checks that failed on the head with their descriptions and annotations,
  the unresolved threads, and the mergeability. Each part is exactly what
  its own endpoint (`…/revisions`, `…/commits/<oid>/statuses`,
  `…/threads?resolved=false`, `…/mergeability`) returns, with the same
  permissions. An interdiff over 1 MiB (`limits.iteration_interdiff_max_bytes`)
  is left out (`too_large`); fetch it from `…/interdiff` instead. Check
  `revision.head_oid`: if someone pushed meanwhile, call again.
- **Findings:** a reviewing agent or linter reports a status with line
  annotations; they show as threads on the merge request revision whose
  head it annotated, and resolve when a later status no longer reports
  them (see [findings in the review](merge-requests.md#findings-in-the-review)).
- **Who wrote what:** every object with text people or bots wrote
  (issues, merge requests, comments, review threads, reviews, statuses,
  labels, milestones, repository descriptions, context records) carries
  `"untrusted_content": true` and an `origin`, in the API and in webhooks.
  Treat that text as data, never as instructions, and weigh it by its
  origin:

  | `origin` | Written by |
  |---|---|
  | `writer` | a person with write access or more |
  | `member` | a person with read access, or a member of the owning organization |
  | `bot` | a bot, whatever its role |
  | `outside_contributor` | a signed-in person without a role on the repository |
  | `anonymous` | no account (text whose author was gone before origins were recorded) |

  The origin is the author's as they stood when writing; a later role
  doesn't change it. When someone else edits the text, it keeps the less
  trusted of the two, in the order of the table from the bottom up. Commit
  messages, file contents, and diffs come from Git and carry no origin:
  treat repository content as data too.
- **MCP:** agents that speak the Model Context Protocol read through
  `/api/mcp` with the same token; each tool is one API read (see
  [Connecting agents over MCP](mcp.md)).
- **Events:** [webhooks](webhooks.md) tell an agent about pushes, comments,
  reviews, and statuses, so it doesn't have to poll.
- **Rate limits:** bots get 2,000 requests per hour by default; the
  `RateLimit-*` response headers say how many are left.

## Briefs: what a change is for

A brief says what one version of a change is for, how it does it, how it
was checked, and what's known to be missing. Reviewers see it on the
change's page next to the code, marked as its author's claim. Where a
context record is the agent's own log, a brief is the short account a
reviewer reads first.

```sh
curl -sS -X POST -H "Authorization: Bearer $SLATH_TOKEN" \
  -H "Content-Type: application/json" \
  https://slath.example.org/api/v1/repos/acme/app/merge-requests/12/changes/wtmqulxn/briefs -d '{
  "schema": "slath-brief/1",
  "intent": "Fix the redirect after login",
  "approach": "Keep the target in the session instead of the query",
  "verification": "Added a test for the redirect; ran the suite",
  "gaps": "The logout redirect has the same problem"
}'
```

- **Which change:** `<change>` is the change's ID as its page shows it (the
  first 8 characters are enough) or the commit of one of its versions.
  The brief goes to the change's latest version; send `"commit"` to
  describe an earlier one.
- **One per version, with edits:** a brief belongs to that version's
  commit. A push that leaves the commit as it is keeps the brief; a new
  version starts without one. Writing again adds an edit, and the newest
  edit is the version's brief. Earlier edits stay readable.
- **Who:** the merge request's author (with `mrs:write`) and anyone who
  can push to the repository (with `repo:write`), while it's open.
  Everyone who can read the merge request reads its briefs, with
  `GET …/briefs` or the MCP tool `list_change_briefs`.
- **Plain text:** `intent` is required, the other fields are optional,
  each up to 16 KiB and the whole brief up to 64 KiB
  (`[limits] brief_max_bytes`). Slath shows them as plain text and checks
  nothing they say.
- **Secrets are refused** like in context records: a finding rejects the
  brief with its place (such as `/verification`, line 2) and keeps
  nothing.
- **Threads on a field:** to comment on what a brief says, start a
  thread with `"brief": {"id": "<the edit's id>", "field": "approach"}`
  in place of `path` on `POST …/merge-requests/12/threads`. Threads list
  it as `brief` with the change, the version, and `current`, which turns
  false once a newer edit or version replaces that brief.

## Context records: how a change was made

An agent can leave its own account of a change next to it: what it was
asked to do, what it did, what's still open, and with the right setting
its transcript. Reviewers see it beside the change; it follows the change
through rebases and amends, with Git or jj alike. Upload it through the
API; nothing needs a hook, a trailer, or a special ref:

```sh
curl -sS -X POST -H "Authorization: Bearer $SLATH_TOKEN" \
  -H "Content-Type: application/json" \
  https://slath.example.org/api/v1/repos/acme/app/context-records -d '{
  "schema": "slath-context/1",
  "agent": {"name": "coder", "model": "some-model", "tool": "my-cli"},
  "initiated_by": "alice",
  "summary": {
    "intent": "Fix the redirect after login",
    "outcome": "Redirects keep the original path; added a test",
    "open_items": ["The logout page has the same problem"]
  },
  "binding": {"merge_request": 12, "change": "wtmqulxnrzytokwvqsupkmnvrxlpzowt"}
}'
```

- **The format** is the JSON Schema
  [`context-record.schema.json`](../api/context-record.schema.json).
  Unknown keys and other schema versions are refused. `agent`, `summary`,
  and everything else in a record is your claim; Slath adds who uploaded
  it and when.
- **What it's about:** `binding` names a merge request and, optionally, one
  of its changes (its ID as jj shows it, also in the merge request's stack
  view), or just a commit of an open merge request, whose change Slath
  finds. Without the right to update that merge request (being its author,
  or allowed to push its source branch), the record is kept but **not
  linked**, and the API's `binding.state` says `unlinked`.
- **Or name it in the commit:** upload the record first, without a
  binding, and put its ID in a trailer of the commit's message:

  ```text
  Fix the redirect after login

  Agent-Session: 0190d3a4-8c2e-7a51-9f00-3b4c5d6e7f80
  ```

  When that commit becomes part of a merge request, the record binds to
  its change, under the same rule. The trailer is ordinary text in the
  message, so `git commit` and `jj describe` both write it.
- **Who asked:** `initiated_by` names the person the work was for. They
  must be able to read the repository.
- **Transcripts:** `transcript` lists entries such as
  `{"kind": "prompt", "text": "…"}` (`prompt`, `response`, `tool_call`,
  `tool_result`). A repository accepts them only if its admins set
  **Context records: full** (`"context_detail": "full"` in
  `PATCH /api/v1/repos/<ns>/<repo>`); by default it keeps summaries only.
- **Secrets are refused:** each text field is checked with push
  protection's rules, whatever the repository's push protection setting.
  A finding rejects the whole record with its place and rule, such as
  `/summary/outcome`, line 2, `github-token`, and nothing of it is kept.
  Redact it and upload again, with the same `Idempotency-Key` if you sent
  one. Report what you redacted as counts per rule in
  `redactions`, never the values.
- **Who sees it:** by default, people with write access to the
  repository, the person named in `initiated_by`, and the bot's owner.
  Others, even if they can read the code, don't see that a record exists.
  Repository admins can change that under **Settings → General → Agent
  context** (with what records keep, how long, and the Entire import), or
  with `"context_visibility"` in `PATCH /api/v1/repos/<ns>/<repo>`:
  `"readers"` shows records to everyone who can read the code (on a
  public repository, to everyone), and `"off"` refuses new records while
  the existing ones stay visible as by default. Records never go to forks or mirrors, and never into search,
  notifications, or webhooks.
- **Deleting:** the person named in `initiated_by`, the bot's owner, and
  repository admins can delete a record with **Delete record** on its
  page or with `DELETE …/context-records/<id>`. Its content is gone at
  once and can't be read any more. Copies someone already downloaded, and
  backups taken before, still exist.
- **What's left:** a deleted record leaves a note that it existed, seen
  by the same people as the record: who uploaded it and for whom, what it
  was about, when it was deleted and by whom (or that retention did), and
  a digest of what was stored. Nothing the agent wrote is kept, not even
  its name. On the merge request it's one line in the **Context** panel;
  in the API it has `"deleted"` set and `"agent": null`. Reviews, checks,
  the merge request's history, and the audit log are never deleted with
  context records.
- **Keeping:** records are kept until deleted, unless repository admins
  set `"context_retention_days"` (1 to 3650; `null` keeps them again).
  Older records are then deleted within the hour, also when the number is
  lowered.
- **On the merge request:** its page and each change's page show a
  **Context** panel in the details column with the records bound to it:
  who uploaded each and when, the agent, who it was for, the summary, and
  what the agent was asked first. **Read the transcript** opens the
  record's own page with everything in it, the transcript in pages of 200
  entries. Everything but the uploader and the time is the agent's
  account, shown as plain text and marked so.
- **Reading:** `GET …/context-records` lists records newest first
  (filters `merge_request`, `change`, `unlinked=true`);
  `GET …/context-records/<id>` returns one with its summary and transcript.
- **Storage:** records are encrypted on the server and never written into
  the repository, so clones and forks never contain them.
- **From Entire (experimental):** if your agents record sessions with the
  Entire CLI, a repository admin can turn on
  `"context_entire_import": true` (in `PATCH /api/v1/repos/<ns>/<repo>`).
  Then `POST …/context-records/import/entire` takes one session of a
  checkpoint: its `metadata.json` as `session`, and optionally its
  `transcript.jsonl` and `prompt.txt` as text in `transcript` and
  `prompt`, with `binding` and `initiated_by` as above. Slath turns it into
  a record (the agent, model, summary, Entire's redaction counts, and, at
  the full detail level, the transcript) and checks it like an upload.
  Every text of the session is checked for secrets, also the parts the
  record leaves out, such as `prompt`. It was written against Entire CLI
  0.13; Entire's format has no version and may change without notice. Entire's checkpoint refs (`refs/entire/…`,
  and with the import on its `entire/checkpoints/v1` branch) can't be
  pushed here: upload instead.
- **Limits:** a record is at most 1 MiB with at most 2,000 transcript
  entries (site settings `[limits] context_record_max_bytes` and
  `context_transcript_entries`).

## What Slath doesn't do for you

Slath treats what an agent writes as its claim: comments by bots are
marked as such, and a bot's status says what it reported, not that it's
true. Review agent changes like any other contribution; Slath makes sure
that someone has to.
