---
version: 1
tools: fs_read, fs_write, shell_exec
---

# review.diff

## When to use it

Before a change goes to a client: a branch about to become a pull request, one
waiting on you, or a patch file in the workspace.

One run reviews one range. Prefer not to run it on a change you wrote in this
session — a second opinion from whoever had the first one is worth less than it
looks; hand it to another identity, or to the human.

## Inputs required and tools it will call

- The range, as two revisions: the base the change will land on and its tip.
  `main..HEAD` is the usual one. Or, if you were handed a `.diff` or `.patch`
  file instead, its path.
- What the change was meant to do, in one line. Without it a review is a list
  of things somebody noticed, not a judgement.

Calls `shell_exec` for read-only git (`diff`, `log`, `show`, `status`),
`fs_read` for the files as they now stand, and `fs_write` for the review.

## Steps

1. Establish the range. `git status`, then `git log --oneline <base>..<tip>`, so
   the review names commits that exist rather than a branch that has moved
   since somebody described it to you.
2. `git diff --stat <base>..<tip>` first, then the diff itself. If the change
   touches more than you were told it would, that is already the first finding.
3. `fs_read` every file the diff changes, at its current state. A hunk is not
   the file: the defect is usually in what the hunk now sits next to.
4. Judge four things, in this order and no other: does it do what it was meant
   to do; is anything in it wrong; is anything in it irreversible once merged —
   a migration, a dropped column, a rotated key; and does it put something in
   the client's repository that should not be there.
5. `fs_write` the review to `.aegis/artefacts/<branch>.review.md`: the range,
   the four answers, then one finding per bullet, each naming `path:line` and
   quoting the line it is about. End with one of *ship*, *change first* or
   *do not ship*.
6. Stop. Merging, pushing, tagging and answering the pull request are not steps
   here, and a later version of this runbook that added them would not make
   them yours.

## How to validate

The review names the exact revisions it read, and `git log` still shows them.
Every finding gives a path and quotes its line. A verdict of *ship* means you
worked through step 4 and found nothing, not that nothing stood out.

## What to return

`skill_return` with `status: done`, the review file in `artefacts`, the range
in `evidence`, and the verdict as the first line of `summary`.
`status: needs_you` when the change turns on a decision that is the client's or
the human's — a behaviour nobody asked for, a dependency with a licence — with
that decision in `open_questions`.

## What requires approval

Every `git` command is an ordinary `shell_exec`, put to the user with its
arguments. Keep them read-only: `diff`, `log`, `show`, `status`. A command that
moves the repository — `checkout`, `merge`, `push`, `reset`, `stash` — is not
part of a review, and the working tree you are reading belongs to somebody who
did not ask you to touch it.

Read-only means the working tree too, and this is where it is easy to be wrong:
a build or a test command **writes**. A filtered `cargo test` in this repository
regenerates a tracked file from a subset of its types and truncates the rest. If
you need the suite to judge the change, say so and let the person run it; if you
run it anyway, you have changed the tree you are reviewing and the review has to
say so.

## What to do if the source is missing

If the range does not resolve, or the patch is not at the path you were given,
return `status: blocked` with what you tried in `open_questions`. Do not review
the conversation's account of the change: a review of a diff nobody read is
worse than no review, because it reads like one.

**A refusal is a missing source.** If a read you needed was denied — by the
person, or by the round limit that ends a turn — the review is partial, and that
is a fact about the review rather than an accident of how it went. Name the file
you could not read, and return `status: needs_you`. Never write *ship* on a diff
you were refused: a verdict on a file nobody opened is the exact failure the
four questions exist to prevent.

If `.aegis/artefacts/` is not there, this workspace has not been set up for the
cabinet. Write the review with its directory created, and say in `summary` that
the shared files are missing — the button is in the project panel.
