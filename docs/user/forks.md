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

## What a fork contains

Only branches and tags. The fork doesn't depend on the original: it keeps
working when the original is cleaned up or deleted. Commits that were only
on branches the original deleted aren't copied, except those of recent
large pushes until the fork's own maintenance removes them.
