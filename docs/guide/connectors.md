# Connectors

A **connector** is an external MCP server: a program Aegis starts and speaks newline-delimited
JSON-RPC to over stdin and stdout. Its tools reach the model beside the built-in ones, named
`<connector-id>__<tool>` — `files__read_text_file`, `git__status`.

**Settings → Connectors** is the only place to add one. No tool installs a connector: naming a
program to start would be `shell_exec` without the dialog.

## Adding one

Give it a name, an **id** (lower-case letters, digits and hyphens; it is the part before `__`), a
**program**, and **arguments one per line**. There is no shell. The MCP filesystem server:

| Field | Value |
| --- | --- |
| Name | `Local files` |
| Id | `files` |
| Program | `npx` |
| Arguments | `-y`, `@modelcontextprotocol/server-filesystem`, `/path/it/may/see` |

Saving starts it. The row shows whether it connected, the server's name, the protocol version and
each tool with its description — or why it failed, with the last lines of its stderr. Nothing
retries on its own; **Reconnect** does.

A connector's tool descriptions are written by the server and ride in every request of the
identities holding its tools. A narrow grant keeps the prompt small as well as the reach.

On Windows, `npx`, `uvx` and similar `.cmd` shims are resolved through `PATHEXT`, as for
`shell_exec`.

## Secrets

A connector names the **environment variables it needs** (`GITHUB_TOKEN`), never their values. Aegis
passes those variables from its own environment, plus the platform minimum (`PATH` and the few each
OS needs to start a process) — not the rest of its environment. Set them where you launch Aegis; the
row says when one is missing. `connectors.json` never holds a secret.

## Granting and the gate

- Installing a connector grants its tools to nobody. Tick them on an identity **one tool at a time,
  by full name**. The built-in Assistant holds every tool, these included.
- **Every connector call asks.** There is no auto-allow and no read-only exemption. The dialog shows
  the connector, the tool, the server's description and the arguments the model wrote, which go to
  the program unchecked. A server's `readOnlyHint` is shown, attributed to the server, and changes
  nothing.
- **Allow for this session** covers that one tool, never the connector: a server can add tools while a
  session is open.

## Not implemented, on purpose

- **No `sampling`, `roots` or `elicitation`.** The handshake advertises no client capabilities, so a
  server cannot make your model generate anything or learn where your workspace is. A server that
  asks anyway gets JSON-RPC "method not found".
- **No resources or prompts.**
- **No HTTP transport** — stdio only.

Images, audio and embedded resources in a result are described rather than inlined, and text is
capped at 64 KB.
