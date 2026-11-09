<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Forks

A fork is your own copy of a repository's branches and tags: you can push
to it freely, without write access to the original, and offer your changes
back with a merge request.

## Forking

**Fork** in the repository header (shown when you're signed in and the
repository allows forks) opens the fork form:

- **Owner**: your account or an organization where you can create
  repositories.
- **Name**: the original's name unless you change it. If you already have
  a repository with that name, the form suggests a free one.
- **Who can see this repository**: the original's visibility at most. A
  fork of a private repository is private, and stays so even if the
  original is deleted.
- **Copy only the default branch**: leaves the other branches behind
  (tags in the default branch's history still come along).

The fork opens right away. Its branches and tags are copied in the
background, usually within a second; until then the page says so and
pushes to it are refused. Merge requests, issues, and the original's
settings aren't copied.

A fork shows **Forked from** with a link to the original (or "a deleted
repository"). **Forks** of a repository (`/<owner>/<repo>/forks`, linked
from the fork form) lists the forks you can see.

## Keeping a fork up to date

On a fork's front page, a line compares its default branch with the
original's: up to date, or how many commits it's ahead and behind. If
it's behind, **Sync fork** brings it forward, without a clone on your side.

If both have their own commits, the branch can't simply move forward:
**Sync fork** explains this and offers to discard your commits (it asks
for your password again). To keep them, push them to another branch
first. Branch protection in your fork applies to syncing like to a push.

With a token (`repo:write`), `POST /api/v1/repos/{owner}/{repo}/sync`
with `{"branch": "main"}` does the same; it answers `fast_forwarded`,
`created`, `up_to_date`, or `diverged` with the counts, and
`"discard": true` drops your commits. `GET .../fork` includes `ahead`
and `behind` for the default branch.

## What a fork contains

Only branches and tags. The fork doesn't depend on the original: it keeps
working when the original is cleaned up or deleted. Commits that were only
on branches the original deleted aren't copied, except those of recent
large pushes until the fork's own maintenance removes them.

## Losing access to the original

A fork of a private (or internal) repository depends on your access to
it. If you lose read access to the original, your fork is locked: nobody
but site administrators can see or clone it. When you get access back,
the fork is unlocked as it was. A fork that stays locked for the
instance's retention period (7 days by default) is deleted. A fork of a
public repository is never locked, even if the original later becomes
private. For a fork in an organization, the access of whoever created it
counts.

## Merge requests from a fork

To propose your fork's changes to the original, open a merge request
against the original with your fork as the source. Through the API:

```text
POST /api/v1/repos/<owner>/<repo>/merge-requests
{"source_repo": "<you>/<fork>", "source_branch": "my-topic", "title": "…"}
```

The source can be any repository of the same fork network that you can
push to. The original receives only the history of that branch, never
your fork's other branches; until you open the merge request it holds
nothing of your fork. Each push to the branch adds a revision, and the
merge request merges like any other. Merging leaves your fork's branch
as it is.

If your fork is less visible than the original (a private fork of a
public repository, for example), everyone who can read the original will
be able to read the merge request's commits. Slath asks you to confirm
that (`"publish_source": true` in the API) and notes it in the merge
request's timeline.

The original's push protection checks every push to the branch before
the merge request takes it over, even if your fork has push protection
off. If it finds a possible secret, the merge request stays at its
previous revision and shows what was found. Remove the secret and push
again, or ask a writer of the original to allow it if it isn't one.

### Edits by maintainers

A merge request from a fork allows edits by maintainers unless you turn
it off (`"allow_maintainer_edits": false` when you open it, or later with
`PATCH` on the merge request). While it's open, anyone who can write to
the original can then push to your merge request's branch in your fork,
for example to fix a typo before merging. They can only update that one
branch: they can't delete it, push other branches or tags, or change
anything once the merge request is closed or merged. Only someone who
can push to your fork can change the setting, and each such push is
recorded in the original's audit log.

The right only goes from the original to forks of it: a merge request
from the original into your fork gives you no access to the original.
