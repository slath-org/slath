<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Merge requests

A merge request asks to merge a branch into another branch of the same
repository, usually the default branch. It works the same whether you use
Git or jj, and it needs no hooks or client configuration.

Merging and line comments come in a later release. For now, merge requests
hold the conversation, the revisions, the commits, and the changes.

## Opening a merge request

Push a branch. Git (or jj) prints a link:

```text
remote: slath: Create a merge request for my-topic:
remote: slath:   https://slath.example/alice/demo/compare?source=my-topic
```

For an hour after the push, the repository's front page, its branch list,
and the merge request list also show a "Recently pushed" banner with the
same link.

The link opens the compare page. It shows the commits and the changes of
the branch against the target, and a form with:

- **Title**: the commit message's first line if the branch has one commit,
  otherwise the branch name.
- **Description**: the repository's template, if the target branch has a
  `.slath/merge_request_template.md`.
- **Reviewers**, **Assignees**, **Labels**, and **Milestone**.
- **Create as draft**: a draft can't be merged, and reviews are requested
  only once you mark it as ready.

You can also pick the branches yourself under **Merge requests → New merge
request**, or open a merge request through the API (`POST
/api/v1/repos/{namespace}/{repo}/merge-requests`).

Only one merge request can be open for the same source and target.
Merge requests share their numbers with issues: `/{namespace}/{repo}/12`
leads to issue 12 or merge request 12, whichever it is.

## Revisions

Every push to the source branch records a new revision, also after a
rebase or an amended commit (`git push --force`, `jj git push`). The
conversation lists each revision; **Compare with previous** shows what
changed since the one before. After a rebase, that includes the changes
that came from the target.

Some merge requests can only be reviewed as a whole, for example when the
branch contains merge commits. The timeline says so at the revision.

## Checking out a merge request

The merge request page shows commands to copy. With Git, the current
revision is always at `refs/merge-requests/<n>/head`:

```sh
git fetch origin refs/merge-requests/12/head && git switch --detach FETCH_HEAD
```

A normal clone or fetch doesn't download these refs.

With jj, fetch the source branch:

```sh
jj git fetch --remote origin --branch my-topic && jj new my-topic@origin
```

## Closing and reopening

The author and people who can triage close a merge request with **Close
merge request**, or **Comment and close**. Deleting the source branch
closes it too. **Restore branch and reopen** recreates the branch at its
last revision; it needs write access to the repository.
