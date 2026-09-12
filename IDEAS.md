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

## Skill runs, turns, and the round cap

Found by hand on 2026-09-03, running the Phase 19 `review.diff` runbook from
Aegis against this worktree's own uncommitted diff. Nothing here is a Phase 19
defect: the pack revealed it, it lives in Phases 13, 16 and 17. The numbers
below are one real trace, in `audit.jsonl` between 18:46:45 and 18:55:52Z.

### 10. A skill run is scoped to a turn, and a real run does not fit in one — landed (A)

**What happened.** One `review.diff` run over a 507-line diff took **5 turns,
41 tool calls and 9 minutes**. `MAX_TOOL_ROUNDS` cut the turn twice — after 16
calls in the first, after 11 in the third. The run's name is a turn local
(`agent/turn.rs:499`, *"A local, so it cannot outlive the turn"*), so:

* 15 of the ~30 calls in the run carry `skill: ""`, **including the `fs_write`
  of the review artefact**;
* the final `skill_return` was **refused** — `E_TOOL_FAILED`, *"no skill is
  running in this turn… a run lasts for the turn that opened it"* — so the run
  never closed, and a person nonetheless got a correct review file on disk;
* `board/trace.rs:141` keys a run on the `skill` field, so the untagged half is
  counted as ordinary session traffic. That is exactly the property PLAN 7.6
  says Phase 17 depends on: *"A run without `skill` on the line cannot be
  budgeted or replayed."*

**The invariant is not wrong everywhere.** A delegated brief and a routine
really are one turn — `handoff::Open` is created around a single `.run()`
(`handoff/runner.rs:262`), with no human between rounds. The scope only strains
in an interactive session, which is the one place a turn can end while the same
work continues. But see § 11: the cap applies to the unattended paths too, and
there nobody can say "continue".

**Three ways out, and they are not substitutes.**

| | What it changes | What it fixes | What it costs |
| --- | --- | --- | --- |
| **A. Run lives on the session** | the local moves to session state; `skill_run` opens, `skill_return` / Stop / cancel closes | attribution, the return, the board, budgets — for interactive runs | the risk `skills/mod.rs` names: a run tagging calls after the conversation moved on. Needs a ceiling (N turns, or an expiry) and cleanup on cancel, session close and restart. **Does nothing for routines.** |
| **B. Accept a late `skill_return`** | the session remembers "last run opened, unreturned"; a later turn may close it. ~30 lines | the visible failure, and the board gets a closing line | the untagged middle stays untagged, so 7.6's property stays broken. A stopgap, not a fix |
| **C. Raise the cap while a run is open** | see § 11 | attribution, the return, the board **and** the unattended path, by keeping the run inside one turn | loosens the runaway protection exactly where a runbook could loop; a 25-round turn re-sends the conversation 25 times |

**A and C both landed**, on the reasoning above: A gives the attribution PLAN
7.6 asks for without pretending a review fits in 8 rounds; C is what makes the
Phase 16 promise ("a scheduler fires a skill") true for a runbook of realistic
length. B was not taken — it fixes the visible failure and leaves the property
Phase 17 depends on broken.

*Landed as:* `Live::run` in `agent/registry.rs` with `open_run` / `carry_run`,
a turn that seeds its local from the session and carries it back, and
`MAX_RUN_TURNS = 4` as the ceiling on the risk that made the scope a turn in
the first place. A cancel closes the run, because pressing Stop is the clearest
statement there is that the conversation has moved on. The refusal at the cap
now tells the model its run is still open, so it does not conclude its
procedure was abandoned. Tests: four in `agent::registry`, three in
`agent::turn`, and the end-to-end one in `tests/skills.rs` that reproduces the
trace — a run opened in one turn, the artefact written and returned in the
next, every line of the second turn on the run's record.

**What we do not know.** Whether a session-scoped run needs to survive an app
restart (probably not: a run whose turn is gone has nothing to return), and
what the ceiling in A should be — that is a measurement, see § 11.

### 11. `MAX_TOOL_ROUNDS = 8` is a number nobody has measured — landed (C)

**What it is.** `agent/turn.rs:99`. The round after the eighth is answered with
`E_TOO_MANY_TOOL_ROUNDS` and the turn ends cleanly. **It is not a permission
gate**: it fires whatever the approval matrix decides, so "allow everything for
this session" does not move it, and neither would any judge in § 12. It is a
runaway-loop bound and a cost bound, and those are the only two things to argue
about when changing it.

**What the one trace says.** 41 calls for a review of a 507-line diff — but
~17 of those were a self-inflicted detour (the model noticed `bindings.ts` had
been truncated, diagnosed it, and repaired it, which was not in the runbook).
A focused run of that same review is closer to **20–25 rounds**. So 8 is not
marginally low, it is low by a factor of three, and raising it to 12 would buy
nothing. Before changing the number, measure the other runbooks the same way —
`deploy.draft` and `alert.draft` are read-heavier and may be worse.

**The unattended path is where this is not cosmetic.** `schedule/runner.rs`
drives the same `Turn`, so a routine that fires a real runbook hits the same
wall with nobody to answer *"answer with what you have, or ask the user to
continue"*. Today, Phase 16 can only schedule runbooks short enough to fit in
eight rounds, and nothing says so.

**Landed as** the third of those: `MAX_TOOL_ROUNDS_IN_SKILL = 24`, chosen by
`round_cap(skill)` per round rather than once per turn, so a run opened on the
third round is judged against the ceiling that fits it from there on. A runbook
declares its tools and its steps, so a bounded procedure is exactly the case
where a higher ceiling is defensible, and every call still passes the gate one
at a time.

**Still open:** the other two options — a setting, and a wall-clock budget for
unattended runs, which need it most and have nobody to say "continue". And the
measurement nobody has taken: what a 24-round turn costs in tokens with the
cache on, which the board's ledger can already answer. Measure it before
raising the number again, and measure `deploy.draft` and `alert.draft` the way
`review.diff` was measured — they are read-heavier and may want more.

### 12. A model that judges how dangerous a call is

Asked directly: could a model watch what the session wants to run, so the
gate could relax? Three different features hide in that question, and only one
of them is available.

**Auto-approving judge — excluded, and not by taste.** PLAN 7.4: *"A verifier
(agent or CI) can raise confidence; it cannot silently flip the default to
auto."* PLAN 7.1 refuses MCP `sampling` with the argument that applies verbatim
here — *"a second agent loop with no session, no identity and no dialog in front
of it"*. Three concrete costs beyond the rule: the audit line becomes *allowed
because a model said so*, which makes the log's value depend on something
non-deterministic that cannot be replayed; the text being judged usually came
from what the session just read (a diff, a README, an alert), so a judge that
reads attacker-influenced content in order to auto-approve is the standard
injection target; and it is a model call in the hottest path there is.

**A judge that can only tighten — compatible.** Raising a risk badge, or
refusing to let a session grant cover a call whose argument shape has drifted,
only ever adds friction. It is allowed by 7.4. It also does not reduce the
number of dialogs, which is what the complaint was about.

**A judge that explains — the one worth building.** One line in the approval
dialog saying what the command actually does, for the case a human misreads a
shell one-liner (`find … -exec` inside a pipe). It does not move who decides.
Costs: latency inside the dialog, and a complacency risk worth naming out loud
("the AI said it was fine") rather than discovering.

**What actually reduces the prompts, with no model in it.** Most of what the
gate asks about is read-only shell. A per-project allow-list of read-only
command *shapes* — `git diff|log|show|status`, `ls`, `rg` — is deterministic,
auditable, and testable, which a judge is not. Do that before considering any
of the above; if it is not enough afterwards, the residue is the honest brief
for a judge.

**The git half of that landed as a tighten, not as the allow-list.** A session
grant on `git` still exists, and still covers `status` / `log` / `diff` /
`show`. A verb that moves the tree (`checkout`, `merge`, `push`, `reset`, …)
offers no grant, so the earlier approval cannot collapse it. Found twice on
`review.diff` against this worktree: 2026-09-03 (`IDEAS.md` § 10) and
2026-09-12, both times `git checkout -- src/ipc/bindings.ts` after a filtered
`cargo test` had truncated that file. The cargo-test bait is still there;
the silent checkout is not.

## Settings, identities, and project scope

Asked directly: Settings feel global, and everything else wants to become
per-project, leaving only the provider credentials as the install-wide
fact. Three different moves hide in that sentence. Two are refused in
`PLAN.md` (§ 7.4, § 7.5, § 7.14). One is this entry.

### 13. Grants live on the identity; two cabinets will fight over them

**The gap.** Settings, identities, memories, connectors and the skill
library are install-global (`store/mod.rs`: seven documents under
application-data). The cabinet (`.aegis/`, `world/`, workspace skills) is
per-project. Routines and sessions *name* a project but live in the
global files. Founding (`PLAN.md` § 7.14) writes a roster in the
workspace and apply writes global identities. That split is load-bearing:
a Reviewer is a Reviewer in the next project too, and apply never widens
a name that exists.

The Settings panel looks like one blob. The pressure that will feel like
"make settings per-project" is not `settings.json`. It is that **the
allow-list is a field on the identity**. A CoS granted `mail__list` for
intake sees mail tools in a delivery session. A Reviewer granted
`shell_exec` for one repo holds it in a watch folder. Role memory that is
actually about a client ("this client wants French") lives in
`memories.json` keyed on the identity, so it follows the CoS into the
next cabinet — which is why `COS.md` *Memory* puts that class of fact in
workspace files.

**What we refused.** Moving `settings.json` / `agents.json` / the keyring
into the workspace. Credentials in a git tree is the forbidden thing.
Identities in `.aegis/` would ship allow-lists (`shell_exec`, connector
tools) with the clone, and a session on another machine would inherit
grants for programs that are not there. A second `settings.json` under
`.aegis/` is a second store with a second schema. Seeding per-project
identity *rows* is the Phase 19 Delivery-identity refusal again.

**The change, when two cabinets share a role and disagree.** Not
per-project settings. Three scopes, of which two already exist:

| Scope | Holds | Today |
| --- | --- | --- |
| Machine / operator | keys, provider roster, MCP programs, skill library, the *definition* of a role | `settings.json`, keyring, `connectors.json`, library `skills/`, `agents.json` minus the allow-lists |
| Identity | perimeter, provider binding | `provider_id`, instructions, role. `AGENTS.md` north star: CoS on one model, specialist on another |
| Project | the world, who is needed here, which grants *here*, which clocks, exec host | `.aegis/`, `world/`, § 7.14 roster, routines that name a project, § 7.12 `exec_host` |

The missing piece is a **binding**: `(identity × project) → tools[],
skills[]`. The identity stays a global row. Apply of a roster writes or
updates *that project's* binding, not the identity. A session in project
A as Reviewer sees A's list. The same Reviewer in project B sees B's. An
identity with no binding in this project is not offered, or is offered
with an empty list (fail closed).

**What it would change.** `Agent::tools` / `Agent::skills` move off the
row, or become the *default* a binding may narrow (never widen without a
Settings act — same as § 7.14 apply). `policy::decide_call` and
`tools::schemas_for` take the open project's id. The Identities form
grows an "in this project" list when a project is open. Routines already
name a project: their standing grants stay on the routine (they already
do). Connector *programs* stay global; a binding names `git__status`, it
does not start git.

**What it would buy.** A cabinet can be narrow without cloning the
Reviewer. Founding (§ 7.14) becomes "write the binding", which is what
the roster file already is, instead of "mint a global identity and hope
the next project does not need it wider". Memories stay per-identity for
*role* facts; client facts stay files (`COS.md`).

**What it is not.** Per-project providers (the key stays in the keyring;
`provider_id` stays on the identity — that is § 9). Per-project
connectors as processes (starting a program is still the operator on this
machine). Per-project *identities* (a fourth CoS per folder is a
generalist that rots, `COS.md` *Roles*). A Settings document inside
`.aegis/`. The per-project allow-list of read-only *command shapes* in
§ 12, which is a policy row on the project, not a binding of an identity.

**Unknown:** whether two projects will actually share a role with
conflicting grants before the provider roster (§ 9) lands. Until they
do, this is theatre — the same test `PLAN.md` § 7.2 uses for several
worlds. § 7.14's "skip names that exist, never widen" is the bandage that
makes that wait cheap. Do not build the binding in order to make
founding look finished.

**When it might come back.** The first time apply of a second roster
wants to grant a tool the existing Reviewer does not hold, *and* taking
it away from the first project would be wrong. Measure that by trying to
found a second cabinet, not by designing the table.
