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

## Writing large files

`fs_write` has no size limit of its own; what bounds it is the turn's output
ceiling, because a file's content is emitted as the call's arguments. That
ceiling now comes from the provider's catalog (128,000 tokens on
`claude-sonnet-5`) instead of motosan's 8192 default, which is what made large
writes fail silently. These are the things that cost us a session to learn and
would cost another to re-derive.

### 4. What a large write actually costs

**Output tokens, and they are the one thing caching cannot touch.** The cache
works on the prompt; a file's content is generated, not re-read. Roughly 100 KB
of arguments is 30,000-odd output tokens — on `claude-sonnet-5` that is more
than the rest of the turn put together, and it recurs in full every time the
file is rewritten.

**JSON escaping is small on ordinary text, and was measured, not guessed.** A
63,000-byte file of regular lines escapes to 64,002 bytes: **+1.6%**. Do not
reach for escaping to explain a large discrepancy — during this session it was
blamed twice for gaps it could not account for, and both times the real cause
was elsewhere. Content dense in quotes, backslashes or very short lines will
sit higher, but nothing like a factor of two.

**The ceiling applies to the escaped arguments, not the file**, which bounds a
single write at roughly 300-350 KB of real content. There is no setting that
moves this; a bigger file has to be written in pieces or generated in place.

**The cheap alternative, when it applies:** content that is *derivable* —
transformed, extracted, computed — costs a few hundred tokens as a script run
through `shell_exec` instead of forty thousand as generated text. This only
helps when the content is not genuinely being authored.

### 5. An approval that expires throws away the expensive part

[`APPROVAL_TTL`](src-tauri/src/approval.rs) is five minutes, and the clock
starts when the call is *requested* — which is after the arguments have been
generated. So a turn can spend several minutes and forty thousand output tokens
writing a file, ask, and lose all of it because nobody was at the screen. The
tokens are spent either way; the refusal recovers nothing.

Observed, not theorised: one test turn asked at 06:38:48 and was refused at
06:43:48 having never been seen.

Nothing recovers a generation once it has happened, so the lever is not the TTL
— it is making a pending approval impossible to miss. The app already owns a
tray icon, which is the obvious place for it. **Unknown:** whether the right
behaviour is a notification, a TTL that does not run while the window is
unfocused, or both; and whether an expensive call deserves different treatment
from a cheap one, which the runtime could know from the argument size it is
already counting for `tool:drafting`. Decide that fresh rather than at the end
of a debugging session.

## motosan-ai backends Aegis is not using

`motosan-ai` 0.27.1 ships two families of backends. Aegis is on the HTTP
family (`anthropic`, `chatgpt-codex`) and reuses a Claude Code / Codex / Grok
CLI *login* as a token source. The CLI family (`claude-code`, `codex-cli`,
`gemini-cli`) is a subprocess that is itself an agent. The `Cargo.toml`
comment already records the distinction; this is the investigation behind it,
so nobody has to re-open the crate docs to decide again.

### 6. Do not turn on `claude-code` / `codex-cli` / `gemini-cli`

**What they are.** `ClaudeCodeProvider` runs `claude --print --output-format
stream-json`. The Codex and Gemini CLI features are the same shape against
`codex exec --json` and `gemini -p "" -o stream-json`. Since motosan 0.25 a
completed CLI turn always reports `stop_reason = end_turn` (never
`tool_use`); `tool_calls` names tools the CLI already ran; tool *results*
stay inside the CLI sandbox and never surface.

**What that would buy, if it bought anything.** A one-line feature flag and
a fifth `AuthKind` that "just works" for anyone who already has `claude` on
PATH. It does not. Aegis *is* the agent loop: the approval gate, the audit
jsonl, workspace policy, `fs_*` / `shell_exec`, handoffs. A CLI backend is a
second harness writing the disk while Aegis still thinks it is deciding.
That is the thing `PLAN.md` § 7.1 forbids (one loop, tools in-process behind
`ToolSpec`) and the thing `AGENTS.md` names: this repo is the harness, not a
new LLM.

The Settings option "Claude Code login on this machine" is **not** this
feature. It is `AuthKind::ClaudeCli`: read `~/.claude`, refresh, POST
`api.anthropic.com` via `Provider::Anthropic`. Tools stay Aegis's. Same
shape as Codex and Grok. That path is already the right use of a CLI
subscription.

**When it might come back.** As an *execution host* later — a disposable
specialist in a worktree, analogue of WSL (`PLAN.md` § 7.12) — not as a
token source. That is a product decision, not a provider.

### 7. Gemini HTTP (`gemini`) as a fifth `AuthKind` — landed

`AuthKind::Gemini`, motosan-ai `gemini` feature, AI Studio key, Settings
picker, probe, and `GET /v1beta/models`. The unknown is settled: motosan
assigns opaque ids (`call_N`) on the stream, and `to_chat_request` remaps
`Role::Tool` onto the function name before the second round. Do not redo
that investigation. What is left of Gemini is § 8.

### 8. Gemini Code Assist as the Claude-Code-login analogue

**The change.** `gemini-code-assist` against `cloudcode-pa.googleapis.com`,
OAuth `ya29.*` from a `gemini auth` already on the machine, plus the GCP
project id from `loadCodeAssist`. New `oauth/gemini.rs` in the same shape as
`claude.rs` / `codex.rs` / `grok.rs`. Billing is the Gemini CLI seat, not
per-token.

**What it buys.** Anyone who already pays for Gemini CLI can point Aegis at
it the way they already point it at Claude Code, without pasting an AI
Studio key. Tools stay Aegis's.

**Unknown:** where the current Gemini CLI actually writes its bundle on
Windows / macOS / Linux, and whether `motosan-ai-oauth` is enough or Aegis
should keep owning the file the way it does for the other three. Read the
file once before choosing.

Do this if the demand is the subscription, not a key. Do not do
`gemini-cli` (the subprocess) for the same reason as § 6.

### 9. The roster is a different piece of work

A fifth `AuthKind` is a Settings row. CoS on one model and a specialist on
another is `provider_id` on the identity, more than one key in the keyring,
and a second arm in `AppState::provider_for`. The trait does not move; the
turn loop does not move. That is the north-star item in `AGENTS.md`
("roster of providers and a per-agent binding"). Gemini HTTP can land
before it. Do not pretend adding Gemini *is* the roster.
