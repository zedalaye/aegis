# Architecture

The WebView renders UI only. The agent loop, tool execution, policy, secrets and audit run in the
Rust runtime, reached through typed Tauri commands and events. `PLAN.md` is the design of record:
the IPC surface (§ 2), the policy matrix (§ 3), the wire protocol (§ 4), platform risks (§ 5) and
everything after the MVP (§ 7).

```
src/                React app — presentation and typed IPC glue only
  ipc/              invoke() / listen() wrappers; bindings.ts is generated from the Rust types
  state/            zustand stores
  lib/              formatting, errors, the markdown parser for previews, drop handling
  components/       layout, chat, sessions, approvals, projects, agents, skills, memory,
                    routines, connectors, board, explorer, settings, audit
src-tauri/
  capabilities/     the window's least-privilege permission set
  src/
    lib.rs          builder, plugins, managed state, the asset scope, run()
    state.rs        application-wide runtime state
    error.rs        the error type every command returns, with stable codes
    commands/       one module per IPC domain
    agent/          turn loop, wire types, transcript projection, event sink, turn registry
      provider/     scripted provider, OpenAI-compatible SSE, motosan-ai dialects, model catalog
    oauth/          tokens written by the Claude Code, Codex and Grok CLIs
    policy/         path resolution and containment, the decision matrix, session grants
    tools/          fs, shell, screenshot, skill, memory, handoff, connector — one ToolSpec registry
    approval.rs     pending approvals: where a turn waits for your answer
    audit.rs        audit.jsonl: one line per tool call
    store/          projects, sessions, agents, memories, routines, connectors, settings documents
    secrets.rs      OS credential store, environment fallback, masking
    skills/         the runbook format, the catalog, the seeded library, proposals
    handoff/        briefs and reports, the bus (parallel, bounded, two attempts), the runner
    schedule/       routines: the door, due times, the unattended runner
    board/          the structured read of STATUS.md, and runs folded from the audit log
    mcp/            MCP client: one stdio JSON-RPC connection per server, and the catalog
    roster.rs       cabinet founding: parse and apply a roster proposal
    compact.rs      derived state for the older half of a transcript
    workspace.rs    the .aegis/ convention: scaffold and the per-request digest
    world.rs        world/: status, the frame, declared sources and drift
    git.rs          work-tree detection and the one `git init`
    exec_host.rs    WSL execution hosts: listing, path translation, probing
    explorer.rs     read-only tree and preview of the workspace
    intake.rs       a dropped file, held by id and copied into .aegis/briefs/
    reveal.rs       open a contained path in the OS file manager
    tray.rs         tray icon and menu
    display.rs      Linux WebView display workarounds
  tests/            integration tests, one file per area
```
