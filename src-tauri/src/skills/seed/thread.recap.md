---
version: 1
tools: fs_list, fs_read, fs_write
---

# thread.recap

## When to use it

Before answering a conversation with more than a couple of messages in it, or
when somebody asks where a thing was left. It works out where the thread stands.

Out of it come three lists: what was agreed, what is outstanding, and what was
asked and never answered. One run recaps one thread, and it recaps a
conversation rather than a relationship — a question this thread does not answer
is a question the recap names, not a reason to go and read the others.

## Inputs required and tools it will call

- The thread, as a path: a directory of messages, an export, or a single file
  with the conversation in it. `fs_list` what you were given first, so the recap
  covers the messages that are there rather than the ones that were mentioned.
- Which side you are. A recap that does not know who "we" is cannot say who owes
  what.

Calls `fs_list` and `fs_read` for the messages and `fs_write` for the recap.
Nothing here runs a command and nothing here answers the thread.

## Steps

1. Read the messages **oldest first**, in the order they were sent. The last
   message is not the state of the thread. A commitment lives where it was made,
   which is usually in the middle, and reading backwards finds the version
   somebody restated instead of the one they made.
2. Count each sentence once. The thread is quoted into every reply, so one
   promise appears eight times over; attribute it to the message that first
   carried it and skip it everywhere else. Eight copies of one commitment read
   as eight commitments, and that is what makes a thread look like a project.
3. Usually the thread is one file and the older messages exist only as the
   quotations inside it. Then say so: those lines are the quoter's copy, pasted
   by somebody with a position, and a mail client trims what it quotes. Mark
   them **as quoted by** whoever forwarded them, and treat a claim that survives
   only in a quotation as weaker than one in a message you have.
4. Sort every claim into exactly one of three. **Agreed**: one side proposed it
   and the other answered yes. **Outstanding**: proposed and not answered, or
   promised and not delivered — with who owes it and since when. **Never
   answered**: a question somebody asked that no later message addresses.
5. Silence is not agreement. A proposal nobody replied to is outstanding, and it
   stays outstanding however reasonable it was and however long ago it was sent.
6. `fs_write` `.aegis/artefacts/recap-<thread>.md`: one line per message — date,
   sender, what changed — then the three lists. Every line in them cites the
   date and sender of the message it came from. `<thread>` is the subject with
   the `Re:` chain taken off, so a conversation gets one recap rather than one
   per round of it.
7. Stop. The recap is what a reply gets written from; it is not a reply, and it
   decides nothing on the outstanding list.

## How to validate

Every line of the three lists cites one message by date and sender. Nothing
appears in two lists. *Agreed* holds only claims with two messages behind them,
the proposal and the answer. The recap ends by saying how many messages it read
and which was the last, so a reader can tell whether it is still current.

## What to return

`skill_return` with `status: done`, the recap in `artefacts`, the number and
range of messages read in `evidence`, and a summary of at most five lines: what
is agreed, what is outstanding, and who is waiting on whom.

`status: needs_you` when the thread turns on something said somewhere else — a
call, a meeting, a message in another channel — with it in `open_questions`.
What was agreed on a call is not in the thread, and a recap that fills that in
is worse than one that says the thread does not contain it.

## What requires approval

The write is an ordinary `fs_write`, put to the user. Reads inside the workspace
happen without asking; a thread stored outside it is a path the person is asked
about. Nothing here replies, forwards, or files anything with the sender.

## What to do if the source is missing

If the path holds no messages, return `status: blocked` and say what you listed.
Do not recap a conversation from the ticket about it, or from what the session
said it contained: a recap is a reading of the messages, or it is a rumour with
dates on it.

If you could read only some of them — a refusal, a format you cannot open, the
round limit that ends a turn — the recap covers those and says so in its first
line, and you return `status: needs_you` when what is missing is where the
answer would be.
