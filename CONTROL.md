# Control channel — messaging face (sketch)

Not a schedule. Not MVP. Do not implement until the `AGENTS.md` "Done when"
line is true. Policy lives in `PLAN.md` § 7.7–7.9; this file is the shape
of the first messaging adapter.

A messaging face is a **UI onto this runtime**, not a second agent, not
remote access, and not inbox intake (mail / WhatsApp are Phase 19). You,
away from the desk, talk to the same process the WebView talks to.

Tailscale is the other inbound path (`PLAN.md` § 7.8): the **same
window**, or later a loopback HTTP/WS on `127.0.0.1` via Serve. This
file is the telegraph (Keybase / X Chat / …). Tailscale is sitting at
the desk. The app does not embed Tailscale and does not supervise
remote Aegis processes over SSH.

```
phone            ── E2EE chat ──►  channel adapter  ─┐
laptop (tailnet) ── window / 127.0.0.1 ─────────────┼─ same commands ─► runtime
desktop WebView  ── invoke / events ────────────────┘
```

Control verbs (`status`, `approve <id>`, `deny <id>`, `halt`) hit
`approval_resolve` / cancel **before** any model sees them. Free text is
`session_send`. Policy, the five-minute approval expiry, and the jsonl
audit do not grow a bypass.

---

## Placement

Lives in `src-tauri/src/channels/` when scheduled — the same kind of named
seam as `mcp/`. Not a `crates/` workspace split. Not a sidecar runtime.

The turn loop already emits to an `EventSink` (`agent/event.rs`).
`WindowSink` is the MVP subscriber. A face is **another sink**, not a
second turn loop and not `app.emit` across every WebView.

```
┌─────────────────────────────────────────┐
│  WebView — status / approve / logs      │
└──────────────────┬──────────────────────┘
                   │ invoke / events
┌──────────────────▼──────────────────────┐
│  src-tauri                              │
│    sessions / tools / policy / audit    │
│    EventSink ─┬─ WindowSink (MVP)       │
│               └─ ChannelHub (later)     │
│                    ├─ Keybase (first)   │
│                    ├─ X Chat            │
│                    ├─ Telegram          │
│                    └─ Discord           │
└─────────────────────────────────────────┘
```

Talk stays in Rust. The frontend never spawns a messenger, never reads
its stdout, never holds its tokens.

---

## Trait (transport-shaped)

Swap the adapter without rewriting the hub. Destination ids and usernames
stay **inside** the adapter; they do not land in session storage.

```rust
#[async_trait]
pub trait ControlChannel: Send + Sync {
    async fn send(&self, conv_id: &str, body: &str) -> Result<(), ChannelError>;
    fn subscribe(&self) -> broadcast::Receiver<Inbound>;
}

pub struct Inbound {
    pub sender_id: String,   // opaque to the hub (Keybase user, X id, …)
    pub conv_id: String,
    pub body: String,
    pub msg_id: String,
}
```

On inbound:

1. Drop non-text, drop the local device's own messages, drop anyone
   outside the allowlist.
2. If the body is a control verb, call the existing Rust functions
   (`approval_resolve`, `session_cancel`, a short status snapshot).
3. Otherwise `session_send`.
4. Replies go back on the same `conv_id`. Coalesce: one message per
   turn, not one per token.

Approval *detail* (paths, shell lines, diffs) is redacted on any face
that is not E2EE. Keybase and X Chat may carry the same summary the
desktop dialog shows.

---

## First adapter: Keybase

Preferred for a high-risk control plane: chat is E2EE with no opt-out,
the client is outbound-only, there is no 500/day cap, and the binary is
already on the machine. Zoom still ships the client in 2026; treat
availability as a Zoom-owned risk, not as "dead, skip it".

Do not bundle Keybase. Do not parse Avdl/Gregor. Do not take the 2021
`keybase-bot-api` crate. Talk to the `keybase` already on PATH, already
logged in.

**Listen** — long-lived child, JSON lines. Supervise and restart it.
No `unwrap` on stdout. On Windows: resolve `keybase.exe` even when a
GUI-launched app has a thin PATH; `CREATE_NO_WINDOW`; the Keybase
service must already be running.

```rust
let mut child = Command::new(&keybase_path)
    .args(["chat", "api-listen"])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()?;
```

**Send** — oneshot is enough for a control channel:

```rust
let payload = serde_json::json!({
    "method": "send",
    "params": {
        "options": {
            "channel": { "name": team, "members_type": "team" },
            "message": { "body": body }
        }
    }
});
let out = Command::new(&keybase_path)
    .args(["chat", "api", "-m", &payload.to_string()])
    .output()
    .await?;
```

`api-listen` events (serde the fields we need, ignore the rest):

```json
{
  "type": "chat",
  "msg": {
    "id": 42,
    "conversation_id": "…",
    "channel": { "name": "harness", "members_type": "team" },
    "sender": { "username": "alice", "device_id": "…" },
    "content": { "type": "text", "text": { "body": "approve abc123" } }
  }
}
```

**Identity.** You cannot 1:1-DM yourself.

| Mode | When |
| --- | --- |
| Existing session + a private team of one (you + your devices) | Zero-setup. Filter on `device_id` so the harness ignores itself. The harness speaks *as you* on that team — keep the team private. |
| Dedicated bot account + paper key in the OS keyring (`keybase --home … oneshot`) | Isolated. Restricted bot possible. 1:1 `you,botname`. |

Zero-setup is fine for a personal box. Isolated is the better control
plane. Never join a public team.

Useful later commands (Rust, not JS): channel status (daemon up?
username? last event), send, set allowlist. Spawn with
`tokio::process::Command`. Do not add `shell:allow-spawn` to
capabilities for this.

---

## Other faces (same trait)

Full comparison: `PLAN.md` § 7.7. Allowed listen shapes are outbound
only: local CLI child, long-poll, Gateway, activity stream. A webhook
or tunnel exposes the runtime on the public internet — forbidden.

| Face | Listen | As a control plane |
| --- | --- | --- |
| **X Chat** | `GET /2/activity/stream`. Not `POST /2/webhooks`. | Other E2EE-shaped adapter. Official Chat XDK (`chat-xdk-core`, git-pinned; crypto only, pair with `reqwest`). Bot account (`xcbot_…` once; `export_keys` blob in the OS keyring — never the WebView, never a user PIN on a server). Coalesce hard: 25 sends / 15 min, 500 / 24 h. Not the Phase 19 social pack, not a Grok-inside-X product. |
| **Telegram** | Bot API long-poll. Not `setWebhook`. | Same trait, better buttons. Cloud chats are not E2EE; bots cannot use Secret Chats. Payload is a reference (`approve abc123`), detail stays on the desktop. |
| **Discord** | Gateway WebSocket. | Same trait, same redaction. Fine as team status, not as a passphrase channel. |
| **WhatsApp** | Cloud API webhook. | **Not a control face.** Intake later. |

A provider switch (`use grok`) is the post-MVP roster, not a channel
command.

---

## Do not

- Start this before MVP is done, even as a head start.
- Bundle a messenger, parse its native protocol, or take an abandoned SDK.
- Let the WebView hold tokens, paper keys, X Chat key blobs, or PINs.
- Speak as the user on a public team or as their personal X account.
- Open an inbound HTTP listener.
- Execute a mutating tool on a chat line with neither allowlist nor `approve`.
