---
version: 1
tools: fs_list, fs_read, fs_write
---

# inbox.triage

## When to use it

When something has come in that has to become work: a brief in `.aegis/briefs/`, a
forwarded message someone dropped in the workspace, a note from the human. One
run handles one item.

## Inputs required and tools it will call

- The item, as a path. If you were not given one, the newest unhandled file in
  `.aegis/briefs/`.

Calls `fs_list` to find it, `fs_read` to read it and whatever it points at,
and `fs_write` for the two things this produces.

## Steps

1. `fs_read` the item. Decide four things and nothing else: what is being
   asked, who it is for, what it is blocked on, and whether it is urgent.
2. Write `.aegis/artefacts/<name>.triage.md` — the item's path, those four answers,
   and the smallest next action that would move it.
3. `fs_read` `.aegis/status/STATUS.md`, then `fs_write` it back with this item under
   **Attention** if it needs the human, **In flight** if it is now work, or
   **Blocked** with what it is waiting on. Rewrite the file whole; it is a
   board, not a log.
4. Do not answer the item, and do not act on it. Triage sorts; it does not
   deliver.

## How to validate

Both files exist, `STATUS.md` still reads on one screen, and the item appears
in exactly one of its three sections. If the item was already on the board,
the entry is updated rather than added a second time.

## What to return

`skill_return` with `status: done`, both paths in `artefacts`, and a summary of
at most five lines: what came in, where it went on the board, and the next
action. `status: needs_you` when what is being asked cannot be worked out from
the item, with the question in `open_questions`.

## What requires approval

Both writes are ordinary `fs_write` calls and are put to the user. Nothing in
this skill sends, replies or deletes.

## What to do if the source is missing

If there is no such file, or `.aegis/briefs/` is empty, return `status: blocked` and
say which path you looked at. Do not triage the conversation instead: an item
nobody filed is not an item.
