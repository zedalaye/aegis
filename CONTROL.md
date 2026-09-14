# Control channel — messaging face (sketch)

Not scheduled. The policy is `PLAN.md` § 7.7–7.9; this file is the shape of the first adapter, for
when one is.

A messaging face is a **UI onto this runtime**: not a second agent, not remote access, and not inbox
intake (a domain pack). You, away from the desk, talk to the same process the WebView talks to.
Tailscale (§ 7.8) is the other inbound path: the same window, or later a loopback HTTP/WS on
`127.0.0.1` through Serve. Aegis does not embed Tailscale and does not supervise remote Aegis
processes.

```
phone            ── E2EE chat ──►  channel adapter  ─┐
laptop (tailnet) ── window / 127.0.0.1 ─────────────┼─ same commands ─► runtime
desktop WebView  ── invoke / events ────────────────┘
```

Control verbs (`status`, `approve <id>`, `deny <id>`, `halt`) reach `approval_resolve` or cancel
**before** any model sees them. Free text is `session_send`. Policy, the approval expiry and the
audit get no bypass.

## Placement

`src-tauri/src/channels/`, a named seam like `mcp/` — not a crate split, not a sidecar. A face is
another `EventSink` beside `WindowSink` (`agent/event.rs`): not a second turn loop, and not
`app.emit` to every WebView. The messenger process, its output and its tokens stay in Rust.

```
┌─────────────────────────────────────────┐
│  WebView — status / approve / logs      │
└──────────────────┬──────────────────────┘
                   │ invoke / events
┌──────────────────▼──────────────────────┐
│  src-tauri                              │
│    sessions / tools / policy / audit    │
│    EventSink ─┬─ WindowSink             │
│               └─ ChannelHub (later)     │
│                    ├─ Keybase (first)   │
│                    ├─ X Chat            │
│                    ├─ Telegram          │
│                    └─ Discord           │
└─────────────────────────────────────────┘
```

## Trait

Transport-shaped, so an adapter can be swapped without touching the hub. Usernames and destination
ids stay inside the adapter and never reach session storage.

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

1. Drop non-text, the local device's own messages, and anyone outside the allowlist.
2. A control verb calls the existing Rust functions (`approval_resolve`, `session_cancel`, a status
   snapshot).
3. Anything else is `session_send`.
4. Replies go back on the same `conv_id`, one message per turn, never per token.

Approval *detail* (paths, shell lines, diffs) is redacted on any face that is not E2EE.

## First adapter: Keybase

Chat is E2EE with no opt-out, the client is outbound-only, there is no daily cap, and the binary is
already on the machine. Zoom still ships it in 2026; its availability is a Zoom-owned risk.

- Use the `keybase` already on PATH and logged in. Do not bundle it, parse Avdl/Gregor, or use the
  2021 `keybase-bot-api` crate.
- **Listen** with a supervised, restarted `keybase chat api-listen` child (JSON lines, no `unwrap`
  on stdout). On Windows: resolve `keybase.exe` despite a GUI app's thin PATH, spawn with
  `CREATE_NO_WINDOW`, and expect the Keybase service to be running already.
- **Send** with a oneshot `keybase chat api -m`.
- Spawn with `tokio::process::Command`. Never add `shell:allow-spawn` to capabilities.

```rust
let mut child = Command::new(&keybase_path)
    .args(["chat", "api-listen"])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()?;

let payload = serde_json::json!({
    "method": "send",
    "params": { "options": {
        "channel": { "name": team, "members_type": "team" },
        "message": { "body": body }
    } }
});
let out = Command::new(&keybase_path)
    .args(["chat", "api", "-m", &payload.to_string()])
    .output()
    .await?;
```

An `api-listen` event (deserialize the fields needed, ignore the rest):

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

You cannot DM yourself, so pick an identity:

| Mode | When |
| --- | --- |
| Existing session + a private team of one (you and your devices) | Zero setup. Filter on `device_id` so the harness ignores itself. It speaks *as you* on that team, so keep the team private. |
| Dedicated bot account + paper key in the OS keyring (`keybase --home … oneshot`) | Isolated; restricted bot possible; 1:1 `you,botname`. The better control plane. |

Never join a public team.

## Other faces

Same trait. Listening must be outbound-only: a local CLI child, long-poll, a gateway socket or an
activity stream. A webhook or tunnel exposes the runtime on the public internet.

| Face | Listen | Confidentiality | Notes |
| --- | --- | --- | --- |
| **Keybase** | local CLI child | E2EE; the server sees metadata, not bodies; stored history has no forward secrecy | first adapter |
| **X Chat** | `GET /2/activity/stream`, not `POST /2/webhooks` | client-side E2EE through the official Chat XDK; no forward secrecy or post-compromise security; X's public-key directory | Bot account; `export_keys` blob in the OS keyring, never the WebView. Send caps (25 / 15 min, 500 / 24 h) force one message per turn. `chat-xdk-core` is crypto only — pair it with `reqwest`; a git dependency as of Aug 2026, so pin a tag; never its WASM binding in the WebView. Revoking OAuth does not invalidate a leaked key blob. Not the social pack. |
| **Telegram** | Bot API long-poll, not `setWebhook` | not E2EE; bots cannot use secret chats | send references (`approve abc123`); details stay on the desktop |
| **Discord** | Gateway WebSocket | not E2EE | team status face, same redaction |
| **WhatsApp** | Cloud API webhook | Meta sees API traffic | **not a control face**; intake only |

Switching providers (`use grok`) is the provider roster, not a channel command.

## Do not

- Bundle a messenger, parse its native protocol, or adopt an abandoned SDK.
- Let the WebView hold tokens, paper keys, key blobs or PINs.
- Speak as the user on a public team or as their personal X account.
- Open an inbound HTTP listener, webhook or tunnel.
- Run a mutating tool from a chat line without both the sender allowlist and an `approve`.
