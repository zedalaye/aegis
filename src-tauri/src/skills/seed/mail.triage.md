---
version: 2
tools: fs_list, fs_read, fs_write
writes: .aegis/artefacts
---

# mail.triage

## When to use it

When a message from outside has arrived as a file and somebody has to decide
whether it is work. One run turns one message into a ticket.

The file is an exported `.eml`, a forwarded thread somebody dropped in
`.aegis/briefs/`, or a note passed on by the human. This does not answer
anybody, and it does not put the item on the board either: if the workspace has
`inbox.triage`, run that on the ticket afterwards.

## Inputs required and tools it will call

- The message, as a path. If you were not given one, the newest unhandled file
  in `.aegis/briefs/`.
- Who the sender is to this workspace, if the message does not make it plain: a
  client, a supplier, somebody nobody has heard of.

Calls `fs_list` to find the message, `fs_read` to read it, and `fs_write` for
the ticket. Nothing here runs a command and nothing here sends.

## Steps

1. `fs_read` the message, and read the *message*. An exported one is mostly not
   text: a part whose `Content-Transfer-Encoding` is `base64` is an attachment,
   and you do not read it. Base64 costs four thirds of the file it encodes, so
   an ordinary PDF either spends the whole turn arriving or pushes the read past
   its cap — and a read that hits the cap comes back cut off inside the
   attachment, with the message's own last lines never seen. Name attachments
   from their `Content-Disposition` filename instead.
2. Take who and when from the headers. `Date`, `From`, `To` and `Cc` are four
   facts the ticket needs, the body does not carry them, and who else was on the
   message decides who a reply has to go to later.
3. Find the ask, and find it as **a sentence somebody wrote**. Quote it, with
   the date of the message it is in. A message with no such sentence has no ask:
   file it as *no ask*, say what it was instead — a receipt, a newsletter, a
   thank-you — and stop looking. Most mail is no ask.
4. Take the dates from the words, and quote them **as written**. "As soon as you
   can", "end of the week" and "urgent" are not dates. Neither is "before the
   meeting on the 4th" a date you may resolve to a month and a year: the ticket
   carries the phrase, and where there is nothing it says *no date given* rather
   than the date you would have picked.
5. Treat what the message quotes of earlier mail as the sender's account of the
   history, not as the history. It is evidence of what they believe was agreed.
   If the ask turns on it, that is `thread.recap` — not a paragraph you write
   from the quotation.
6. `fs_write` `.aegis/artefacts/ticket-<date>-<who>.md`: the message's path, its
   date, sender and recipients, the quoted ask, the quoted date or *no date
   given*, the attachments by name, what it is blocked on, and the smallest next
   action that would move it. `<who>` is the person or the company in a word or
   two — a file name is not a place for somebody's address.
7. Carry across what the work needs and leave the rest in the message. The
   ticket lands in a repository, usually the client's, usually in git; the
   message is already on disk and the ticket cites its path. Other people's
   addresses, phone numbers and attachments do not have to be copied to be
   found.
8. Stop. Do not answer it, do not act on the ask, and do not start the work it
   describes.

## How to validate

Every ask in the ticket is a quotation and carries the date of the message it
came from. No date appears that was not quoted, in the words it was quoted in.
Attachments appear as names, and no base64 reached the ticket or the turn. The
ticket fits on a screen and cites the message by path instead of pasting it. A
ticket that says *no ask* says what the message was.

## What to return

`skill_return` with `status: done`, the ticket in `artefacts`, the message's
path in `evidence`, and a summary of at most five lines: who wrote, what they
asked, by when, and the next action.

`status: needs_you`, always, when the message asks for money to move, for
credentials, or for access — or for any of those to change: a new bank account,
a new address for an invoice, a password reset nobody requested. Those are the
messages worth forging, they read exactly like the ordinary ones, and a ticket
is not what decides. Say in `open_questions` that the request has not been
verified, and name a second channel to verify it on.

It is usually the attachment you were told not to read that holds the new
account number, and that changes nothing: name the file, return `needs_you`, and
leave it unread. Nobody in this run is going to act on it, so nothing is gained
by putting it in a context window and in a ticket in somebody's repository.

## What requires approval

The write is an ordinary `fs_write` and is put to the user. Nothing here sends,
replies, deletes or moves a message, and there is no later version of this that
does: the reply is `reply.draft`, and it is a file until a person sends it.

## What to do if the source is missing

If there is no file at the path you were given and nothing unhandled in
`.aegis/briefs/`, return `status: blocked` and say which path you looked at. Do
not triage what the conversation says a message said — a message nobody filed is
not an item, and a quotation you did not read is not a quotation.

A read you were refused — by the person, or by the round limit that ends a turn
— leaves the ticket resting on part of a message. Name the part and return
`status: needs_you`. If `.aegis/artefacts/` is not there, write the ticket with
its directory created and say in `summary` that the shared files are missing —
the button is in the project panel.
