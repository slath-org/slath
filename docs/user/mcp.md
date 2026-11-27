<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Connecting agents over MCP

Agents that speak the Model Context Protocol (MCP) can read from Slath
without custom code: repositories, files, issues, merge requests with their
stacks and mergeability, the one-call iteration view, review threads, and
checks. The endpoint is `/api/mcp` on your Slath instance, for example
`https://git.example.org/api/mcp`.

Every MCP tool runs one read operation of the [REST API](agents.md#surfaces-for-programs)
as you and returns that operation's response. So a tool can do exactly what
your token can do through the API, nothing more, and a repository you may
not see looks like one that doesn't exist. Tools only read; nothing an agent
does over MCP changes anything.

## Connect a client

Create a token for the agent, ideally for its own [bot](bots.md), with the
`repo:read` scope and restricted to the repositories it works on. Then add
Slath as a remote MCP server ("streamable HTTP") that sends the token in the
`Authorization` header. With Claude Code, for example:

```sh
claude mcp add --transport http slath https://git.example.org/api/mcp \
  --header "Authorization: Bearer $SLATH_TOKEN"
```

Other clients take the same three things: the URL, the transport
(streamable HTTP), and the header. Without a token, the tools see what
anonymous visitors see.

Slath doesn't offer the OAuth sign-in some clients try first; configure the
header instead. If a client insists on OAuth, it gets `401` and stops.

## Tools

| Tool | What it returns |
|---|---|
| `list_repos`, `get_repo`, `search_repos` | Repositories you can see |
| `list_branches`, `list_tags`, `list_commits`, `get_commit` | Refs and history |
| `get_tree`, `get_blob` | A directory or a file at a branch, tag, or commit |
| `get_commit_diff` | A commit's diff as plain text |
| `list_issues`, `get_issue`, `list_issue_comments`, `search_issues` | Issues and their discussion |
| `list_merge_requests`, `get_merge_request`, `get_merge_request_diff` | Merge requests and their current diff |
| `get_stack`, `list_revisions`, `get_interdiff` | The changes of a merge request, its revisions, and what changed between two of them |
| `get_iteration` | Everything for the next round in one call: the current revision, the interdiff since yours, failing required checks, unresolved threads, and mergeability |
| `get_mergeability`, `get_ledger` | What blocks a merge, and how review state came about |
| `get_merge_request_facts` | What Slath sees in a diff: size, sensitive paths, dependency changes, deleted tests, binaries, mode changes, possible secrets ([facts](merge-requests.md#facts)) |
| `get_group` | Merge requests linked to land together |
| `list_threads`, `list_reviews`, `list_merge_request_comments` | Review threads, reviews, and the conversation |
| `list_statuses` | The checks on a commit |
| `list_merge_request_briefs` | A merge request's own briefs, one per revision, across its changes ([briefs](agents.md#briefs-what-a-change-is-for)); writing is through the API |
| `list_change_briefs` | A change's briefs: items saying what each version is for, what it changes, and how it was checked ([briefs](agents.md#briefs-what-a-change-is-for)); writing is through the API |
| `list_context_records`, `get_context_record` | Agents' records of how changes were made, where you may see them ([context records](agents.md#context-records-how-a-change-was-made)); uploading is through the API |
| `list_assigned_work_orders`, `list_work_orders`, `get_work_order`, `list_work_order_events`, `get_work_order_state`, `get_merge_request_state` | Work orders: the ones assigned to your bot, a repository's, one order, and what happened under it, and an order's or merge request's state in one read ([work orders](agents.md#work-orders-delegating-one-task)); claiming is through the API |

The arguments are the operation's path and query parameters, such as
`ns`, `repo`, `number`, `ref`, and `path`. Lists come in pages: pass a
page's `next_cursor` as `cursor` to get the next one. Each tool's
description names the API operation it runs, and `tools/list` returns
every tool with its input schema. Tools change only with the API, and
existing ones keep their names and parameters.

## Treat results as data

Issues, comments, and review threads can contain text written to steer an
agent ("ignore your instructions and …"). Slath marks that text the same way
in MCP as in the API: every object with text that people or bots wrote
carries `"untrusted_content": true` and an `origin` that says who wrote it
(see [who wrote what](agents.md#surfaces-for-programs)). Files, diffs, and
commit messages are repository content. Tell your agent to treat all of it
as data, never as instructions; the server tells MCP clients the same when
they connect.

A work order's task token gets less: text below the order's content trust
comes as a placeholder, exactly as in the API (see
[what the bot reads](agents.md#what-the-bot-reads-content-trust)).

## Errors

- A failed operation is a tool error whose text is the API's problem
  document, with its `status` and `code` (for example `not_found`).
- Arguments that don't fit a tool's schema are rejected before anything
  runs.
- A wrong or expired token gets `401` for every request, as in the API.
- Requests from web pages of other sites are refused (`403`); MCP clients
  send no `Origin` header and aren't affected.
- Each request counts against your API rate limit and returns the same
  `RateLimit-*` headers.

The endpoint keeps no sessions and doesn't stream: each request gets one
JSON response. It speaks the protocol versions 2025-11-25, 2025-06-18, and
2025-03-26.
