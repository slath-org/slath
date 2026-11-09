<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Merge requests

A merge request asks to merge a branch into another branch of the same
repository, usually the default branch. It works the same whether you use
Git or jj, and it needs no hooks or client configuration.

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

## Changes

Slath tracks each commit of a merge request as a **change**, so reviews
can follow a commit through rebases, amends, and rewrites. The
**Commits** tab shows each commit's change ID. It uses jj's format: for
commits made with jj, it's the ID `jj log` shows; for commits made with
Git, it's the ID jj would give that commit. Nobody needs hooks, trailers,
or any client configuration.

When you push a new revision, Slath matches each commit to a change, in
this order:

1. **IDs.** jj's `change-id` header, a `Change-Id:` trailer (as Gerrit
   tools write them), or the ID jj gives a Git commit. A Git commit that a
   jj user later edits keeps its change, because jj writes exactly that ID
   into the edited commit. IDs only count within the same merge request.
2. **Identical patch.** After a plain `git rebase`, the commits are new
   but their patches are the same.
3. **Similar content.** A commit without a known ID whose patch is close
   to a change that's no longer in the stack, for example a jj commit
   edited with `git rebase -i`. These are marked **Inferred**. They never
   carry approvals over, since the patch changed.
4. Otherwise, a **new** change.

Pushing to the source branch reports this right away, in the same form
for `git push` and `jj git push`:

```text
remote: slath: merge request !12 for my-topic, revision 3: https://slath.example/alice/demo/merge-requests/12
remote: slath:   kxyzabcd unchanged          Add the parser
remote: slath:   lmnopqrs updated (version 3) Handle empty input
remote: slath:   nopqrstu new                Add a flag
remote: slath:   zyxwvuts removed
```

If recording the revision takes longer than a few seconds, the push only
prints the link, and the merge request updates a moment later.

If two equally similar changes could fit, the commit becomes a new change
and the Commits tab says which ones it may continue. The timeline sums up
each push, for example "rebased onto main, 2 changes modified, 1 added".

**Unresolved commits** block merging: a commit whose header and trailer
name different changes, or two commits with the same ID. The author, or
anyone who can push the source branch, picks a change for each of them on
the Commits tab. The choice is kept for the next pushes.

Each change ID links to the change's page. It lists the change's versions
and how each was matched (by its ID, by an identical patch, by similar
content, or by hand), with the change's history, and compares any two
versions:

- **Exact**, when both versions have the same parent: the diff is
  exactly what changed between them.
- **Diff of patches**, when the parents differ (after a rebase): the two
  patches are compared, so the rebase itself doesn't show. **Tree diff,
  including the rebase** shows everything that's different.

**Compare with previous** on a revision in the timeline compares two
revisions with `git range-diff`. That's marked **heuristic**: it pairs
commits by similarity and may pair them differently than the change IDs.
The tree diff between both revisions is one link away.

On the same tab you can also **split** a commit off into a new change,
when it was matched wrongly, or attach a new change to one that was
removed. Both are recorded in the timeline and the audit log, and can be
undone the same way.

## Reviews

Below the conversation, **Review revision N** submits a review of the
current revision: a comment, an approval, or a request for changes. The
author can only comment. Each reviewer's latest approval or request for
changes is the one that counts.

The author and people who can triage ask for reviews under **Ask for
reviews**, by username or as `organization/team`. Requested reviewers find
the merge request under **Review requests** (the icon next to
notifications) until they review it.

When you review again later, the merge request offers the changes since
your last review.

Slath keeps a **ledger** of every such decision: how each commit was
matched to a change, which approvals were kept or went stale and why,
which statuses stopped counting because the head moved, when the target
moved, and which signatures a rebase merge removed. It's append-only;
`GET …/merge-requests/{n}/ledger` returns it.

What a new revision does to approvals depends on the branch protection of
the target: by default, an approval stays only if the new revision kept
every change's patch, as after a plain rebase. Otherwise it's marked stale
and no longer counts. The timeline notes either case.

Every merge request page opens with a **readiness card**: one headline
(such as "Ready to merge", "Approval required", or "Conflicts with
main"), the reason, and the one thing you can do next, chosen for you:
**Merge**, **Review**, **Ask for review**, **Mark as ready**, or **Resolve
conflicts**. Below it, each requirement of the target branch is a line
marked done (✓), waiting (clock, for normal waiting such as missing
approvals or running checks), or blocking (✗, something must change).
Open a line to see why and what fixes it. The same card sits again above
the merge and review forms at the bottom, so you don't have to scroll up.
The API returns the requirements at `GET …/merge-requests/{n}/mergeability`.

The timeline stays in one order, oldest first; **Jump to newest** skips
to the end. When you come back after reviewing, the card says how many
entries are new since your review, and a divider marks where they start.
Runs of label, assignee, milestone, and review-request changes fold into
one line you can open. Comments by bots are marked as claims: they never
look like a review.

**More** next to the title marks a merge request as draft or ready, and
closes or reopens it.

## Comments on lines

A **thread** starts on one line or a range of lines of a revision's diff,
on either side (the new text, or the old text a change removed), or on the
merge request as a whole. Others answer in it, and the merge request's
author, the thread's author, or anyone who can push resolves it. Branch
protection can require every thread to be resolved before merging.

Comments can wait in your **pending review**: only you see them until you
submit the review, and then they appear together.

When a new revision comes in, a thread moves along with its lines if they
are unchanged (marked "from revision N"). If they changed, the thread is
**outdated**: it stays in the conversation and on the revision it was made
on, but never sits on a line it wasn't written about.

On the **Changes** tab, select a line number to open a comment form under
that line; set the last line to comment on a range. It works without
JavaScript and with touch. Threads show under the lines they're about,
with a link to answer in the conversation. **Mark viewed** collapses a
file you've reviewed; it opens again by itself when the file changes in a
new revision.

In the conversation, each thread shows the code it's about, its replies,
and whether it's outdated or resolved. **Reply** sends an answer right
away; **Add to my review** keeps it pending. **All**, **Unresolved**, and
**Mine** above the timeline narrow the conversation to open threads or to
what you wrote.

The API has `GET …/merge-requests/{n}/threads` (with each thread's current
position), `POST …/threads` (`path`, `side`, `line_start`, `line_end`,
`pending`), `POST …/threads/{id}/comments`, and `PATCH …/threads/{id}`
with `{"resolved": true}`.

## Merging

When every requirement is met, the readiness card offers **Merge** to
people who can merge (the write role). It leads to the merge form at the
bottom, with the strategies the repository allows:

- **Fast-forward** moves the target to the branch's head; only possible
  when the target has nothing the branch doesn't.
- **Rebase** (the default) replays each commit on the target. Authors,
  messages, and unknown headers stay as they were; the committer is the
  person merging, and signatures are dropped because the commits change.
  Each commit carries its change's ID, so jj users who fetch see the same
  IDs as the merge request, even for commits made with Git.
- **Merge commit** adds a commit with both parents ("Merge !12: title"
  unless you write another message).
- **Squash** makes one commit from the first commit's author, with the
  title and description as the message unless you write another. It keeps
  the change's ID if there's one change, and gets a new one otherwise.

Merging uses the head you looked at: if someone pushed meanwhile, it stops
and shows the new revision. If someone pushes to the target while merging,
Slath rebuilds the merge on top, a few times at most. **Delete the branch
after merging** removes the source branch unless it's protected.

Messages you write for merge commits and squashes go through the
repository's push protection like pushed commits do.

When the branch conflicts with the target, the merge box lists the
conflicting files and the commands that resolve them with Git or jj.

The API merges with `POST …/merge-requests/{n}/merge` and
`{"strategy": "rebase", "expected_head_oid": "<head you reviewed>"}`.

## Statuses and required checks

CI systems and other tools report the result of a check on a commit with
`POST /api/v1/repos/{namespace}/{repo}/statuses/{commit}` and a token with
the `statuses:write` scope:

```json
{"state": "success", "context": "ci/build", "target_url": "https://ci.example.org/runs/42"}
```

`state` is `pending`, `success`, `failure`, or `error`; the latest status
per context counts. Every status shows who reported it: Slath knows the
reporter, not whether the check really ran. The commit page and the
merge request's **Checks** list them, and
`GET …/commits/{commit}/statuses` returns them.

When the target's branch protection requires a context, the merge box
shows it, with the reporter and why a status didn't count:

- only statuses on the merge request's head commit count;
- never the author's own statuses, or those of bots the author owns;
- if the rule lists reporters for the context, only theirs.

## Reading diffs

Diffs look the same on commits, the compare page, and merge requests:

- In a changed line, the characters that changed are highlighted and
  underlined. Lines are paired with the most similar line of the hunk;
  lines that changed almost entirely, and very long lines, are shown
  without highlighting.
- A file tree above the files links to each of them.
- **Split** shows old and new side by side; it's offered on screens at
  least 1024 pixels wide. **Ignore whitespace** hides changes that only
  touch spaces and indentation. Once you're signed in, both choices are
  remembered.
- Files marked as generated in the repository's `.gitattributes`
  (`linguist-generated` or `slath-generated`) are collapsed; **Load diff**
  shows them.
- Large diffs load as you scroll, and very large files on request.

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
