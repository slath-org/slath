<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Merge requests

A merge request asks to merge a branch into another branch of the same
repository, usually the default branch. It works the same whether you use
Git or jj, and it needs no hooks or client configuration.

## Working with Git or jj

Every step works the same with Git and with jj: nothing to install or
configure beyond the repository's address and your credentials (an SSH
key, or a personal access token from **Settings → Access tokens** as the
HTTPS password). The commands below use `https://slath.example/alice/demo.git`;
use your repository's address from **Clone**. Each pair shows the same
step with Git, then with jj.

Get the repository:

```sh git clone
git clone https://slath.example/alice/demo.git demo && cd demo
```

```sh jj clone
jj git clone https://slath.example/alice/demo.git demo && cd demo
```

Commit your work on a branch and push it. The push prints the link to
open the merge request (see [Opening a merge request](#opening-a-merge-request)):

```sh git push-branch
git switch -c my-topic
git add -A && git commit -m "Add notes"
git push origin my-topic
```

```sh jj push-branch
jj commit -m "Add notes"
jj bookmark create my-topic -r @-
jj git push --bookmark my-topic
```

Update it after review by pushing again: each push is a new revision.
Amending and rewriting are fine; the merge request follows each change
across revisions:

```sh git update
git add -A && git commit --amend --no-edit
git push --force-with-lease origin my-topic
```

```sh jj update
jj squash --into my-topic
jj git push --bookmark my-topic
```

When the target moved on, bring the branch up to date by rebasing it on
the target and pushing (or use [Update branch](#updating-the-branch) on
the server). If the rebase stops at a conflict, resolve it and continue
as the comment says:

```sh git rebase
git fetch origin
git rebase origin/main
# On a conflict: resolve the files, then git add <files> && git rebase --continue
git push --force-with-lease origin my-topic
```

```sh jj rebase
jj git fetch
jj rebase -b my-topic -d main@origin
# On a conflict: resolve it (jj resolve, or edit the files), then push
jj git push --bookmark my-topic
```

After a change on the server (**Update branch**, applied suggestions, a
maintainer's edit), take the new revision before you change more:

```sh git catch-up
git pull --rebase origin my-topic
```

```sh jj catch-up
jj git fetch
```

After the merge request is merged, continue from the updated target:

```sh git after-merge
git switch main && git pull origin main
```

```sh jj after-merge
jj git fetch && jj new main@origin
```

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

To merge a branch of your fork into the original, see
[Merge requests from a fork](forks.md#merge-requests-from-a-fork).

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
**Commits** tab of a merge request shows each commit's change ID. It uses jj's format: for
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
and the Commits page says which ones it may continue. The timeline sums up
each push, for example "rebased onto main, 2 changes modified, 1 added".

**Unresolved commits** block merging: a commit whose header and trailer
name different changes, or two commits with the same ID. The author, or
anyone who can push the source branch, picks a change for each of them on
the Commits page. The choice is kept for the next pushes.

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

On the same page you can also **split** a commit off into a new change,
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

When you approve, Slath notes how many files of the revision's diff you
had marked viewed, counting only files you viewed as they are in that
revision. The approvals line shows it for each approval, for example
"Approved by alice (r3, 12 of 15 files viewed)", and opens to list the
files no approver had viewed. It's a snapshot: viewing files later
doesn't change it, and an approval kept after a rebase keeps it. Apart
from this snapshot, which files you marked viewed stays private to you.
The mergeability API returns the same as `coverage`.

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

On the **Changes** page, select a line number to open a comment form under
that line; set the last line to comment on a range. It works without
JavaScript and with touch. Threads show under the lines they're about,
with a link to answer in the conversation. **Mark viewed** collapses a
file you've reviewed; it opens again by itself when the file changes in a
new revision.

In the conversation, each thread shows the code it's about, its replies,
and whether it's outdated or resolved. **Reply** sends an answer right
away; **Add to my review** keeps it pending. The **Show** menu above the
timeline narrows the conversation to **Unresolved** threads or to what you
wrote (**Mine**).

The API has `GET …/merge-requests/{n}/threads` (with each thread's current
position), `POST …/threads` (`path`, `side`, `line_start`, `line_end`,
`pending`), `POST …/threads/{id}/comments`, and `PATCH …/threads/{id}`
with `{"resolved": true}`.

## Suggestions

In a comment on lines of the new side, a `suggestion` block proposes the
text that replaces them (an empty block deletes them):

````text
Shorter:

```suggestion
let total = items.len();
```
````

People who can push to the branch, and the merge request's author,
apply suggestions on the server, one at a time or several together, in
one of two ways, the same for Git and jj users:

- **As a new commit** on top of the branch: its author is whoever made
  the suggestion (the person applying when several people did), its
  committer the person applying, and its message ends with a
  `Co-authored-by` line for each suggester.
- **Folded into its change**: the commit that introduced the lines gets
  the fix, with a `Co-authored-by` line for each suggester, and the
  commits after it are rebased onto it. Every change keeps its ID,
  authors and messages stay, and the person applying becomes their
  committer. It works only when the suggested lines come from one commit
  of the merge request and nothing later conflicts; the result is always
  the same as applying on top. A branch that blocks force pushes takes
  only new commits.

In the conversation, each suggestion shows the lines it replaces and its
text. **Apply suggestion** applies it right away; **Add to batch** collects
several, which **Apply batch** in the merge box applies together. The
merge box also chooses between a new commit and folding. Either way the
result is a revision like any push, and the threads of applied
suggestions are resolved. After a fold, `jj git fetch` shows the
same changes with the fix; with Git, reset your branch to the new one
(`git pull --rebase`) before you push again.

A suggestion can be applied only while its lines are unchanged since it
was made; once they changed, it's outdated. Suggestions changing the
same lines can't be applied together, and what a suggestion adds goes
through the repository's push protection. The branch's protection
applies as for a push.

The API applies them with `POST …/merge-requests/{n}/suggestions/apply`
and `{"comments": ["<comment id>", …], "mode": "commit", "expected_head_oid": "<head>"}`
(`"mode": "fold"` to fold).

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

## Merge when ready

While a requirement is still missing (an approval, a check, a resolved
thread), people who can merge can choose **Merge when ready** with a
strategy instead. The merge request then merges as them as soon as the
last requirement is met: when a check passes, a review approves, a thread
is resolved, it's marked ready, or the target moves. It merges once, and
a notice in the merge box says who set it.

It's cancelled when someone other than the person who set it pushes a
new revision (their own pushes keep it), when the merge request is
closed, and with **Cancel merge when ready**, which the setter, the
author, and anyone who can merge may use. If the merge itself fails, for
example on a conflict, the timeline says why. The person who set it gets
a notification when it merged, failed, or was cancelled.

The API sets it with `PUT …/merge-requests/{n}/auto-merge` and
`{"strategy": "rebase"}` (or `{}` for the repository's default), and
cancels it with `DELETE` on the same path; merge requests show it as
`auto_merge`.

## Updating the branch

When the target has commits the branch doesn't, the merge box offers
**Update branch** to people who can push to the source branch. On a fork
merge request, writers of the target can too while edits by maintainers
are allowed. Choose how:

- **Merge** the target into the branch: adds one merge commit, and
  nothing you pushed changes. Pull before you push again.
- **Rebase** the branch onto the target: replays its commits like a
  rebase merge, keeping authors, messages, and change IDs. jj picks the
  new commits up as the same changes with `jj git fetch`; with Git, reset
  your branch to the new one (`git pull --rebase`) before you push again.

The update becomes a new revision, and the timeline says who updated the
branch and how. The source branch's protection applies as for a push: a
branch that blocks force pushes can only be merged into, not rebased.

The API updates with `POST …/merge-requests/{n}/update-branch` and
`{"method": "merge", "expected_head_oid": "<head you saw>"}`; the
mergeability response says `"up_to_date": false` when there's something
to update.

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

A status can point at code with up to 50 line annotations (findings), for
example a linter's warnings or an evaluator's comments:

```json
{"state": "failure", "context": "lint", "annotations": [
  {"path": "src/lib.rs", "line_start": 12, "line_end": 14, "severity": "warning",
   "category": "maintainability", "message": "This branch is never taken."}
]}
```

- `path` is a file of the commit, and its lines must exist there (at most
  500 per annotation; `line_end` defaults to `line_start`).
- `severity` is `notice`, `warning`, or `failure`.
- `category` is `correctness`, `security`, `tests`, `dependencies`,
  `scope`, `maintainability`, `docs`, or `other` (the default).
- `message` is plain text of up to 4 KiB.

A status with one invalid annotation is rejected as a whole, and the
error names it (`annotations[3].line_start`). Annotations are returned
with their status and sent in the `status` webhook. The limits are
`limits.status_annotations` and `limits.status_annotation_max_bytes`.

### Findings in the review

Every open merge request of the same repository whose revision head is
the annotated commit shows the annotations as **finding threads** on that
revision, written by the reporter (with its Bot badge) and marked with the
status's context and the severity:

- The message shows as plain text: no Markdown, links, mentions, or
  suggestions, since it comes from a tool.
- A later status of the same context on that commit resolves the findings
  it no longer reports ("resolved by" the reporter), and reopens those it
  resolved that way if they come back. A finding a person resolved stays
  resolved.
- Findings stay on their revision. After a new push they show in the
  conversation with their revision, not in the new diff; statuses on the
  new head bring the new revision's findings.
- You can reply to and resolve findings like any thread, but they never
  count as a review or approval, send no notifications, and don't count
  for "resolved threads" in branch protection. Whether a check blocks the
  merge depends only on its status.
- Statuses reported in a fork never reach a merge request in the original
  repository: like required checks, findings are read from the target
  repository. The API marks finding threads with `finding` (`context`,
  `severity`, `category`).

## Reading diffs

Diffs look the same on commits, the compare page, and merge requests:

- In a changed line, the characters that changed are highlighted and
  underlined. Lines are paired with the most similar line of the hunk;
  lines that changed almost entirely, and very long lines, are shown
  without highlighting.
- A file tree above the files links to each of them. It lists the files
  with changed content; **All files** adds the folded ones (see below),
  each with the reason it's folded.
- **Split** shows old and new side by side; it's offered on screens at
  least 1024 pixels wide. **Ignore whitespace** hides changes that only
  touch spaces and indentation. Once you're signed in, both choices are
  remembered.
- Only files with changed content are shown right away. Everything else
  is folded with its reason, and **Load diff** shows it where there are
  lines to show: renamed or copied files without changes, files whose
  mode is the only change, files where only whitespace changed, files
  marked as generated in the repository's `.gitattributes`
  (`linguist-generated` or `slath-generated`), binary files, symbolic
  links, submodules, and files cut at the line limit. Above the diff,
  Slath says how many files are folded and how many are past the file
  limit, so nothing is left out without saying so.
- Large diffs load as you scroll, and very large files on request.

### As plain text

For tools, scripts, and agents, diffs also come as plain text:

| What | In the browser | Through the API |
| --- | --- | --- |
| A commit's diff (against its first parent) | `/<ns>/<repo>/commit/<oid>.diff` | `GET /api/v1/repos/<ns>/<repo>/commits/<oid>/diff` |
| A commit as a patch mail | `/<ns>/<repo>/commit/<oid>.patch` | `GET /api/v1/repos/<ns>/<repo>/commits/<oid>/patch` |
| A merge request's diff (current revision, from its merge base) | `/<ns>/<repo>/merge-requests/<n>.diff` | `GET /api/v1/repos/<ns>/<repo>/merge-requests/<n>/diff` |
| A merge request's commits as patch mails | `/<ns>/<repo>/merge-requests/<n>.patch` | `GET /api/v1/repos/<ns>/<repo>/merge-requests/<n>/patch` |

Diffs are what `git diff` prints and patches what `git format-patch`
prints, so `git apply` and `git am` take them. They're never folded or
cut, up to 64 MiB. Who can read the repository can read them; the API
needs a token with `repo:read` for private repositories.

## Checking out a merge request

**Check out** next to the merge request's title shows commands to copy,
each with a copy button, for Git or for jj. Slath shows your tool's: the
one you chose under **Settings → Appearance → Commands for**, or, set to
automatic, the one your recent pushes came from (jj writes a change ID
into every commit, Git doesn't). Until it knows, it shows Git; switching
to jj on the page works at any time, and your browser remembers it. Only commands differ; every page looks the
same whichever tool made the commits. With Git, the current revision is
always at `refs/merge-requests/<n>/head` (here merge request 12):

```sh git checkout
git fetch origin refs/merge-requests/12/head && git switch --detach FETCH_HEAD
```

A normal clone or fetch doesn't download these refs. With jj, which
fetches branches, fetch the source branch:

```sh jj checkout
jj git fetch --remote origin --branch my-topic && jj new my-topic@origin
```

A merge request from a fork is the branch `mr/<n>` in the original for
those who check it out, so neither tool needs the fork as a remote; see
[The branch mr/\<n\> in the original](forks.md#the-branch-mrn-in-the-original).

## References

Issues and merge requests share numbers, so `#N` (or `owner/repo#N` for
another repository) in a description or comment links to whichever it
is, showing its title and state on hover. The issue or merge request you
mention shows "mentioned this in" with a link back, once for each place
that mentions it. You see that line only if you can read where the
mention was written, and mentions of something you can't read stay plain
text.

## Closing issues

A closing keyword right before an issue reference closes the issue once
the work lands on the default branch:

```text
Fixes #12
closes: owner/other-repo#3
```

The keywords are `close`, `closes`, `closed`, `fix`, `fixes`, `fixed`,
`resolve`, `resolves`, and `resolved`, in any case, optionally followed
by a colon. Put one before each issue: in `Fixes #1, fixes #2` both
close, in `Fixes #1 and #2` only `#1` does. They count in:

- commit messages pushed to the default branch, by Git, jj, or a merge
  in Slath (up to `[limits] review_max_commits` commits per push);
- the description of a merge request when it's merged into the default
  branch.

The issue closes as completed, and its timeline links the commit or
merge request for everyone who can read it. A keyword acts only if
whoever pushed or merged may close that issue: keywords naming an issue
you can't close, or a repository you can't read, do nothing.

## Closing and reopening

The author and people who can triage close a merge request with **Close
merge request**, or **Comment and close**. Deleting the source branch
closes it too. **Restore branch and reopen** recreates the branch at its
last revision; it needs write access to the repository.

## Notifications

Your notifications list merge requests next to issues, marked `!N`. You
hear about a merge request when:

- you're asked to review it (**Review requested**). A draft asks no one
  yet: its requested reviewers hear about it once it's marked ready
  (**Ready for your review**);
- you opened it (**You opened it**), or took part: commented, reviewed,
  wrote in a review thread, are assigned, or are a requested reviewer
  (**You took part**);
- someone mentions you, or a team you're in (**You were mentioned**);
- you watch the merge request or the repository (**You're watching**).

Taking part notifies you of new comments, submitted reviews, new
revisions, readiness, merging, closing, and reopening. Comments of a
review that isn't submitted yet stay quiet until it is. You never hear
about what you did yourself, and someone who can no longer read the
repository hears nothing more. Every email has an unsubscribe link for
that merge request; ignoring a repository still lets review requests and
mentions through.
