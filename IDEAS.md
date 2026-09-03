# IDEAS.md — deferred work

> **Role.** Things worth doing that we deliberately did not do, with enough of
> the reasoning to judge them again later. Not a backlog, not a plan.
>
> | Question | File |
> | --- | --- |
> | What we might do, and why we did not | **this file** |
> | When, in what order, after MVP | `PLAN.md` § 7 |
> | Stack, MVP scope, permissions | `AGENTS.md` |
>
> An entry earns its place by being *checkable*: what it would change, what it
> would buy, and what we do not know. An entry nobody can act on without
> redoing the investigation is a note, not an idea — write the investigation
> down or delete the entry.

## Prompt caching

Turns on Anthropic go to `/v1/messages` and ask for the cache with three
breakpoints: the tool schemas, the system prompt, and the newest message
(`agent/provider/motosan.rs`, `to_chat_request`). What follows is what that
left on the table.

**Measured baseline, 2026-09-03.** A `claude-sonnet-5` session over an OAuth
login, doing code execution and file reads, settles around **74%** of input
tokens served from cache. Effective input cost is roughly a third of the
uncached price. Any idea below should be judged against that number, and the
number should be re-measured before anyone acts on one — the badge in the
session header reports it.

### 1. Teach `Role::Tool` to carry a cache breakpoint

**The gap.** `motosan-ai` serializes a tool result without ever consulting
`message.cache`, though the `User` and `Assistant` arms beside it do. So the
breakpoint on the newest message skips a tool result and lands on the assistant
turn that asked for the call (`mark_cache_breakpoint`). The cost is one round
of lag: each tool result is paid at full price once, *then* written to the
cache, then read. With PDF text and CSV content coming back through `fs_read`,
that one full-price pass is probably most of the missing 26 points.

**The change.** Build the block, then mark it — symmetric with the arms above
it:

```rust
Role::Tool => {
    if let Some(tool_use_id) = &message.tool_call_id {
        let mut block = json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": message.content,
        });
        if message.cache {
            block["cache_control"] = json!({"type": "ephemeral"});
        }
        messages.push(json!({"role": "user", "content": [block]}));
    }
}
```

`tool_result` is a legitimate `cache_control` target — the API accepts it
alongside `text`, `image`, `tool_use` and `document`. This is a case that was
never wired, not a workaround.

**Two things that will trip whoever does it.** The arm exists *twice* in
`providers/anthropic.rs` — around line 329 for the API-key path and around 684
for the OAuth path, which rebuilds its messages separately. A patch to one of
them changes nothing for a session on a Claude Code login. And on the Aegis
side the whole change is deleting the `find` that skips `Role::Tool` in
`mark_cache_breakpoint`: the mark then goes on the last message, full stop.

**Not an alternative: routing around `Role::Tool`.** `ContentBlock`
(`motosan-ai/types.rs`) is `Text | Image | Document`. There is no way to spell
a `tool_result` block through the `content_blocks` path that *does* honour
`cache`, so avoiding the role would mean being unable to answer a tool call at
all. The role is right; the line is missing.

**How to try it.** `[patch.crates-io]` onto a local fork gives a measurement in
an afternoon and costs nothing to throw away. Upstream PR is the clean version
but its landing date is not ours. **Unknown:** whether the 26 points hide 15
recoverable ones or 3. Measure before investing in the PR.

### 2. The system prompt is one cache block, and its tail moves

`system_message` (`agent/transcript.rs`) appends the shared digest and the
world block at the *end* of the system prompt, and `workspace::digest` is
rebuilt on every request by design — so that a round which writes `DECISIONS.md`
sees it in the next one. Caching is a prefix match, so on a workspace using the
convention, any write into `.aegis/` invalidates the system breakpoint *and the
entire conversation behind it*. The tool schemas survive, because they render
first and hold their own breakpoint; nothing else does.

Splitting `system` into blocks would only move the problem — the stable half
would stay readable, but everything after the volatile tail, which is the whole
transcript, would still fall. The shape that actually holds is to stop carrying
per-request state in the system message and put it *after* the conversation,
in the last user turn, where the caching guidance says volatile content
belongs. That is a real change to `transcript::build` and to the `WireMessage`
shape, and it is worth it only on workspaces that write to `.aegis/` mid-turn.

**Unknown:** how often that actually happens in a working session. Instrument
before rebuilding — a counter on how many requests see a changed digest would
settle it in a day.

### 3. What is not recoverable, so nobody chases it

The first turn of a session has nothing to read and can only write. The last
round of every turn writes an entry that is read only if the session
continues. Both are structural: a session's cached share has a ceiling below
100% no matter what, and a short session has a lower one than a long session.

Related gotcha, not an idea: the minimum cacheable prefix on `claude-sonnet-5`
is 1024 tokens. Below it the API ignores `cache_control` silently — no error,
no cache entry. A short session reporting 0% may simply be too small to cache,
and that is correct behaviour rather than a regression.
