---
version: 1
tools: fs_read, fs_write
---

# reply.draft

## When to use it

When somebody outside is owed an answer and a person will send it. The reply is
written as a file, with a source named under every claim in it.

It does not send, and there is no later version of it that does.

## Inputs required and tools it will call

- The ticket, as a path — the one `mail.triage` wrote. If you were not given
  one, that is the end of the run. A reply drafted to whatever was written most
  recently is how the wrong client gets answered, and it will read perfectly.
- The recap, if the thread has one, and the workspace's
  `.aegis/status/STATUS.md` and `.aegis/decisions/DECISIONS.md`. Those are where
  a commitment is allowed to come from.

Calls `fs_read` for those and `fs_write` for the draft. It lists nothing, runs
nothing, and sends nothing.

## Steps

1. `fs_read` the ticket, then the recap if there is one. Answer the ask the
   ticket quotes — not the question you would rather they had asked, and not the
   four other things you noticed on the way past.
2. Before writing a word, decide what the reply commits to. A date, a price, a
   scope, a name, an order of work: each is a commitment, and each needs a file
   behind it — the decisions ledger, the board, a plan `deploy.draft` wrote, or
   the ticket's own quotation.
3. A commitment with nothing behind it does not get softer wording. "I'll look
   into it", "should be fine", "early next week" are commitments in the reader's
   hands, and the reader is right. Leave it out of the draft and put it in
   `open_questions`.
4. Write the draft: who it is to, the subject, the body. Short, in the language
   the message was written in, answering the quoted ask in its first two lines.
   It goes to everyone the message went to — the ticket lists them — because
   taking somebody off a thread is a decision about who gets to see the answer,
   and it is not one this runbook makes on the way past.
5. Under the body, a **sources** block: one line per claim in the reply, naming
   the file it came from. It is not part of the message — it is what the person
   reviewing reads instead of reconstructing the reply from scratch.
6. `fs_write` it beside the ticket, with `.reply` before the extension:
   `.aegis/artefacts/ticket-<date>-<who>.reply.md`.
7. Stop. Sending is the human's, after `never-send-without-review` — this draft
   is what that runbook was seeded for, so run one into the other. Do not send
   it, do not schedule it, and do not tell anybody it is on its way.

## How to validate

Every sentence of the body either answers the quoted ask or has a line in the
sources block. No date, price or scope is in the reply that is not in a file the
sources block names. The draft answers one message; if it answers two, it is two
drafts.

## What to return

`skill_return` with `status: done`, the draft in `artefacts`, the ticket and the
files behind the sources block in `evidence`, and a summary of at most five
lines: what the reply says and what it commits to.

`status: needs_you` when the answer turns on something that is not in a file — a
price, a deadline, whether to take the work at all — with it in `open_questions`
and the draft left unwritten. A draft that guesses at the price is a draft
somebody sends.

## What requires approval

The write is an ordinary `fs_write`, put to the user. There is no tool here that
sends, and a mail connector installed later does not change that: a connector
replaces where a message comes from, not the gate it goes out through
(`PLAN.md` § 7.6). A reply is a file until a person sends it.

## What to do if the source is missing

No ticket, no run: return `status: blocked` and ask for the path. Do not draft
from the session's account of what the client wrote. The quotation is the whole
point, and a reply written from a summary of a message is a reply to a message
that does not exist.

If the ledger or the board is missing, say so and draft only what the ticket
supports — a workspace where nothing has been decided in writing is a fact the
summary should carry. A read you were refused, by the person or by the round
limit that ends a turn, is a source the block cannot name: leave the claim out
and return `status: needs_you`.
