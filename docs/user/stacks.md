<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Stacks

A stack is a merge request of several commits, each one step of the
work: first the parser, then its use, then its documentation. Slath
reviews each commit as a [change](merge-requests.md#changes) of its own,
follows it through every rewrite, and lets the bottom of a stack land
while the rest is still in review. It works the same with Git and with
jj, also when both are used on the same stack, and needs nothing on your
machine beyond the repository's address and your credentials.

The commands below use `https://slath.example/alice/demo.git` and the
branch `my-stack`. Each pair shows the same step with Git, then with jj.
To get the repository, see [Working with Git or jj](merge-requests.md#working-with-git-or-jj).

## Pushing a stack

Commit each step on its own, bottom first, and push the branch:

```sh git stack-push
git switch -c my-stack
git add parser.txt && git commit -m "Add the parser"
git add main.txt && git commit -m "Use the parser"
git push origin my-stack
```

```sh jj stack-push
jj commit -m "Add the parser" parser.txt
jj commit -m "Use the parser"
jj bookmark create my-stack -r @-
jj git push --bookmark my-stack
```

Then open the merge request from the link the push prints (see
[Opening a merge request](merge-requests.md#opening-a-merge-request)).
Its **Changes** tab lists both changes, bottom first, and each change has
its own page, reviews, and checks.

## Changing a commit in the middle

Edit the files, then fold the edit into the change it belongs to and push
again. Commits above it are rebased and stay the same changes; reviews
of their unchanged patches still count:

```sh git stack-fix
git commit -a --fixup ":/Add the parser"
git rebase --autosquash origin/main
git push --force-with-lease origin my-stack
```

```sh jj stack-fix
jj squash --into 'subject("Add the parser")'
jj git push --bookmark my-stack
```

`:/Add the parser` names the newest commit whose message matches, and
`subject("Add the parser")` the change with that subject; a commit ID
(Git) or change ID (jj) works too. With jj you can also edit the change
in place: `jj edit <change>`, change the files, and push.

## Adding a change on top

Commit and push again. With jj, move the bookmark to the new top first:

```sh git stack-grow
git add -A && git commit -m "Document the parser"
git push origin my-stack
```

```sh jj stack-grow
jj commit -m "Document the parser"
jj bookmark move my-stack --to @-
jj git push --bookmark my-stack
```

If you pushed the stack [without naming a branch](merge-requests.md#without-naming-a-branch),
push to the same `refs/for/<target>/<topic>` with Git. With jj, `jj git
push --change @-` would push the new top as a branch of its own and open
a second merge request; move the bookmark the first push created
(`push-…`, shown by `jj log`) as above instead.

## Taking over a stack

Anyone who can push the branch can continue the stack, with either tool,
whichever tool made its commits:

```sh git stack-take-over
git fetch origin
git switch my-stack
```

```sh jj stack-take-over
jj git fetch
jj bookmark track my-stack@origin
jj new my-stack
```

The steps above then work as before, and changes keep their identity
across both tools. jj writes its change ID into each commit it rewrites,
also into a commit first made with Git, and Git keeps that ID when it
rebases or amends the commit. A commit only Git ever touched has no ID of
its own: after a rebase, Slath finds it again by its identical patch;
after an amend, by its similar content, marked **Inferred** (see
[Changes](merge-requests.md#changes)). A rewrite beyond recognition
becomes a new change.

## Catching up

When someone else pushed the stack, or Slath rewrote it (part of it
landed, **Update branch**, or a suggestion folded into its change), take
the new version before you change more:

```sh git stack-catch-up
git pull --rebase origin my-stack
```

```sh jj stack-catch-up
jj git fetch
jj new my-stack
```

## Landing the bottom of a stack

When the lower changes are ready and the rest isn't, land them, either
in Slath or by pushing. The merge request stays open with the rest.

In Slath, use **Land up to here** on the page of the highest change that
should land (see [Landing part of a stack](merge-requests.md#landing-part-of-a-stack)).
With a rebase or squash, Slath rebases the rest onto what landed: catch
up before you push again.

By push, move the target to the highest change that should land; here
the second change of three, so the target moves to the commit below the
top:

```sh git stack-land-next
git push origin my-stack~1:main
```

```sh jj stack-land-next
jj bookmark set main -r my-stack-
jj git push --bookmark main
```

That works when the stack sits on the current target, so the push is a
fast-forward; otherwise catch up, rebase onto the target, and push the
stack first. The landed changes show as **Landed**, and the next push of
the branch no longer counts them.

## Landing the whole stack

Merge it in Slath, or push the stack's top to the target:

```sh git stack-land-all
git push origin my-stack:main
```

```sh jj stack-land-all
jj bookmark set main -r my-stack
jj git push --bookmark main
```

The merge request is then merged by whoever pushed (see
[Merging by pushing the target](merge-requests.md#merging-by-pushing-the-target)).
Delete the branch when you're done; a branch pushed
[without naming one](merge-requests.md#without-naming-a-branch) is
deleted by Slath:

```sh git stack-done
git switch main && git pull origin main
git branch -d my-stack && git push origin --delete my-stack
```

```sh jj stack-done
jj bookmark delete my-stack
jj git push --deleted
jj new main
```

Working alone, that's the whole cycle: push the stack, look through it
in Slath (or let CI check it), and land it by push, all at once or from
the bottom.

## Stacks from a fork

A stack in your fork works the same: push it to your fork and open the
merge request against the original (see
[Merge requests from a fork](forks.md#merge-requests-from-a-fork)). To
take what moved on in the original, rebase the stack onto it as in
[Keeping a fork up to date](forks.md#keeping-a-fork-up-to-date); the
changes keep their identity. Maintainers of the original continue a
fork's stack on the branch `mr/<n>`, with the steps above (see
[The branch `mr/<n>` in the original](forks.md#the-branch-mrn-in-the-original)).
