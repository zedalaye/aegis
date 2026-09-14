# IDEAS.md — deferred work

Things worth doing that we chose not to do yet, with enough of the investigation to judge them
again. Not a backlog: an entry says what it would change, what it would buy, and what we do not
know. Entries are numbered and cited from code (`IDEAS.md § 12`); keep the numbers.

Order and scope live in `PLAN.md` § 7 and `AGENTS.md`.

## Prompt caching

Anthropic turns go to `/v1/messages` with three cache breakpoints: the tool schemas, the system
prompt and the newest message (`agent/provider/motosan.rs`, `to_chat_request`).

**Baseline, 2026-09-03:** a `claude-sonnet-5` session over an OAuth login, reading files and running
code, serves about **74%** of input tokens from cache. Re-measure (the session header shows it)
before acting on anything below.

### 1. Teach `Role::Tool` to carry a cache breakpoint

**Gap.** `motosan-ai` serializes a tool result without reading `message.cache`, so the newest-message
breakpoint skips tool results (`mark_cache_breakpoint`) and lands on the assistant turn before them.
Each tool result is paid in full once before it is cached. With large `fs_read` results, that is
probably most of the missing 26 points.

**Change.** Mark the `tool_result` block, which the API accepts as a `cache_control` target:

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

Then delete the `find` that skips `Role::Tool` in `mark_cache_breakpoint`.

**Trips.** The arm exists twice in `providers/anthropic.rs` — the API-key path (around line 329) and
the OAuth path (around line 684) — and patching one changes nothing for the other. Avoiding the
role is not an option: `ContentBlock` cannot express a `tool_result`.

**Try it** with `[patch.crates-io]` onto a local fork, then upstream. **Unknown:** how many of the
26 points are recoverable.

### 2. The system prompt's tail moves

The workspace digest and the world block are appended to the end of the system prompt and rebuilt
on every request, so any write under `.aegis/` invalidates the system breakpoint and the whole
conversation behind it; only the tool schemas survive. Splitting the system prompt only moves the
problem. What holds is carrying per-request state after the conversation, in the last user turn —
a real change to `transcript::build` and `WireMessage`.

**Unknown:** how often the digest changes mid-session. Count it before rebuilding.

### 3. What is not recoverable

The first turn can only write the cache; the last round of every turn writes an entry that is read
only if the session continues. Short sessions have a lower ceiling. Below 1,024 tokens
(`claude-sonnet-5`) the API ignores `cache_control` silently, so 0% on a tiny session is correct.

## Writing large files

### 4. What a large write costs

- **Output tokens**, which caching cannot reduce: about 100 KB of arguments is 30,000 output tokens,
  paid again on every rewrite.
- **JSON escaping is small** on ordinary text: +1.6% measured on a 63 KB file. It was wrongly blamed
  twice for larger gaps.
- **The output ceiling applies to escaped arguments.** It comes from the provider catalog (128,000
  tokens on `claude-sonnet-5`), which bounds one write at roughly 300–350 KB. Larger files need
  pieces or generation in place.
- **Derivable content** is cheaper as a script run through `shell_exec`.

### 5. An expired approval throws away the expensive part

`APPROVAL_TTL` (`src-tauri/src/approval.rs`) is five minutes from the request, which comes after the
arguments were generated. A turn can spend minutes and 40,000 output tokens on a file and lose them
because nobody was at the screen (observed: asked 06:38:48, refused 06:43:48, never seen).

The lever is making a pending approval hard to miss: a tray notification, a TTL that pauses while
the window is unfocused, or different treatment for expensive calls (the runtime already counts
argument size for `tool:drafting`). **Unknown:** which.

## motosan-ai backends Aegis does not use

Aegis uses the HTTP backends (`anthropic`, `chatgpt-codex`, `gemini`) and reuses CLI *logins* as
token sources. The CLI backends are subprocesses that are agents themselves.

### 6. Do not enable `claude-code` / `codex-cli` / `gemini-cli`

They run the CLI (`claude --print --output-format stream-json`, `codex exec --json`,
`gemini -p "" -o stream-json`), always report `end_turn`, name tools the CLI already ran, and keep
tool results inside the CLI's sandbox. That is a second harness writing to disk while Aegis thinks
it decides — what `PLAN.md` § 7.1 forbids.

The "Claude Code login" setting is not this: `AuthKind::ClaudeCli` reads `~/.claude`, refreshes, and
calls `api.anthropic.com`; tools stay Aegis's. The CLI backends might return as an *execution host*
(a disposable specialist in a worktree, like WSL in § 7.12), which is a product decision.

### 7. Gemini HTTP as an `AuthKind` — landed

`AuthKind::Gemini` with an AI Studio key, the Settings option, the probe and `GET /v1beta/models`.
Settled: motosan assigns opaque call ids (`call_N`), and `to_chat_request` maps `Role::Tool` onto
the function name before the next round. What remains is § 8.

### 8. Gemini Code Assist as the Claude-Code-login analogue

**Change.** `gemini-code-assist` against `cloudcode-pa.googleapis.com`, with the OAuth token of an
existing `gemini auth` and the GCP project id from `loadCodeAssist`; a new `oauth/gemini.rs` shaped
like `claude.rs`, `codex.rs` and `grok.rs`.

**Buys.** Gemini CLI subscribers can use Aegis without an AI Studio key.

**Unknown:** where the current CLI writes its bundle on each OS, and whether `motosan-ai-oauth`
should own that file. Not the `gemini-cli` subprocess (§ 6).

### 9. The provider roster is separate work

A CoS on one model and a specialist on another needs `provider_id` on the identity, several keys in
the keyring and a second arm in `AppState::provider_for`. The trait and the turn loop do not move.
A new `AuthKind` is a Settings row, not the roster.

## Skill runs, turns and the round cap

Found on 2026-09-03 running `review.diff` against this worktree's diff: one run took 5 turns, 41
tool calls and 9 minutes. The round cap cut it twice, half the calls lost their `skill` tag
(including the artefact write), and the final `skill_return` was refused.

### 10. A skill run outlived its turn — landed (A)

The options were **A**, the run lives on the session; **B**, accept a late `skill_return` (fixes the
visible failure, leaves the middle of the run untagged); **C**, a higher round cap while a run is
open. A and C landed.

*As built:* `Live::run` in `agent/registry.rs` (`open_run` / `carry_run`); a turn seeds the run from
the session and carries it back; `MAX_RUN_TURNS = 4`; Stop closes the run; the cap message tells the
model its run is still open. Tests in `agent::registry`, `agent::turn` and `tests/skills.rs`.

**Open:** whether a run should survive an app restart (probably not).

### 11. `MAX_TOOL_ROUNDS = 8` was never measured — landed (C)

The round cap bounds runaway loops and cost; it is not a permission gate, and no grant moves it. A
focused `review.diff` needs 20–25 rounds, and routines hit the same wall with nobody to say
"continue".

*As built:* `MAX_TOOL_ROUNDS_IN_SKILL = 24`, chosen per round by `round_cap(skill)`.

**Open:** a setting; a wall-clock budget for unattended runs; what a 24-round turn costs with caching
(the board's ledger can answer); round counts for `deploy.draft` and `alert.draft`.

### 12. A model that judges how dangerous a call is

Three features hide in the question:

- **A judge that auto-approves — excluded.** `PLAN.md` § 7.4: a verifier can raise confidence, not
  flip the default. It would make the audit depend on a non-replayable model, read attacker-influenced
  text in order to approve, and sit in the hottest path.
- **A judge that only tightens** is allowed, but it does not reduce prompts.
- **A judge that explains** — one line in the dialog saying what a command does — is worth building.
  Name the complacency risk ("the AI said it was fine").

**What reduces prompts without a model** is a deterministic allow-list of read-only command shapes.
The `git` half has landed as a tighten: the `git` session grant covers only read-only verbs, with no
option before the verb, none of `--output`, `--no-index`, `--contents` or `--ext-diff`, and not in a
folder laid out like a bare repository (`policy/matrix.rs`). The previous deny-list of tree-moving
verbs let `git -c core.fsmonitor=<program> status` and `git config` run under the grant (review of
2026-09-14); before that, `git checkout -- src/ipc/bindings.ts` had slipped through twice after a
filtered `cargo test` truncated that file (2026-09-03, 2026-09-12).

**Open:** a per-project allow-list for other read-only shapes (`ls`, `rg`).

## Settings, identities and project scope

### 13. Grants live on the identity; two cabinets will conflict

**Gap.** Settings, identities, memories, connectors and the skill library are install-global; the
cabinet (`.aegis/`, `world/`, workspace skills) is per-project. The pressure that will feel like
"per-project settings" is that **the allow-list is a field on the identity**: a Reviewer granted
`shell_exec` for one repository holds it in a watch folder.

**Refused.** Moving `settings.json`, `agents.json` or keys into the workspace (credentials in git,
allow-lists shipped with a clone, a second store), and per-project identity rows.

**The change, when needed:** a **binding** `(identity × project) → tools[], skills[]`. The identity
stays a global row; applying a roster writes the project's binding; `policy::decide_call` and
`tools::schemas_for` take the open project's id; routines keep their own standing grants; connector
programs stay global.

| Scope | Holds |
| --- | --- |
| Machine | keys, provider roster, MCP programs, skill library, role definitions |
| Identity | perimeter, provider binding, instructions |
| Project | the world, who is needed here, grants here, clocks, execution host |

**Not:** per-project providers (§ 9), per-project connector processes, per-project identities, a
settings file under `.aegis/`, or the command-shape allow-list of § 12.

**Unknown:** whether two projects will ever need one role with conflicting grants. Until then,
"skip names that exist, never widen" (`PLAN.md` § 7.14) is enough. Find out by founding a second
cabinet, not by designing the table.

## Transcript display

### 14. Markdown in the chat bubble, with a sanitizer

**Gap.** Assistant text is a `<p>` with `white-space: pre-wrap`; fences, lists and headings stay
punctuation.

**Why not yet.** Model output is untrusted, and the WebView is the whole UI, approval dialog
included.

**Change.** A parser, not `dangerouslySetInnerHTML`: reuse the typed-tree approach of
`src/lib/markdown.ts` (built for § 7.15). Raw HTML stays inert, remote images do not load, and links
are text until a Rust command opens them in the OS browser. `Message.text` stays a plain string.

**Trips.** Streaming: an unclosed fence mid-stream looks broken if re-parsed on every token —
debounce, or render it as `<pre>` until it closes. Links: an `<a href>` can navigate the app away or
hit `tauri:` / `file:`.

**Unknown:** whether the sanitizer stays closed as CommonMark extensions are added. Start when a long
fenced reply is genuinely unreadable, and ship the sanitizer in the same change.

## Seeing the workspace

### 15. A workspace explorer is not an IDE — landed (`PLAN.md` § 7.15)

The operator needed to see what the agent can already `fs_list` and `fs_read`. What stays
load-bearing: no editor, no save path from the WebView, no `file://`, no `fs:` or opener plugin.
What was a slogan and was dropped: "a code repository already has an editor" (a project is not
assumed to be code) and "the convention directories are the tree" (brief inputs are paths anywhere
in the workspace).

### 16. A dropped file is a brief, never an artefact — landed (`PLAN.md` § 7.15)

`.aegis/briefs/` is work going in; `.aegis/artefacts/` is work coming out, written under the gate. A
new source declared in `world/sources.yml` is a third destination (re-perception), not a drop
target. The drop copies, keeps both files on a name clash, does not wrap the file in markdown, does
not start a turn, and does not infer a runbook from the extension.
