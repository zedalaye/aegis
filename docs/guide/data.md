# Data and execution hosts

## Where your data lives

Aegis keeps its own records in seven JSON documents in the application-data directory:

| Platform | Path |
| --- | --- |
| Windows | `%APPDATA%\dev.aegis.harness\` |
| macOS | `~/Library/Application Support/dev.aegis.harness/` |
| Linux | `~/.local/share/dev.aegis.harness/` |

| Document | Holds |
| --- | --- |
| `projects.json` | project names, workspace paths, execution hosts |
| `sessions.json` | conversations: messages, tool calls, the identity each session runs as, tokens spent per turn |
| `agents.json` | the identities you made. The built-in Assistant is a constant in the runtime, not a row |
| `memories.json` | what each identity remembers. Deleting an identity deletes its memories |
| `routines.json` | what is on a clock, its standing approvals, and today's run count |
| `connectors.json` | external MCP servers: id, program, arguments, and the *names* of the environment variables they need |
| `settings.json` | the provider roster: for each provider an id, a label, the authentication kind, base URL and model id |

- **No document holds a key**, and none records whether something is running: after a crash a
  session comes back idle and a connector comes back disconnected.
- The documents are readable and safe to edit while Aegis is closed. One that does not parse is
  renamed `<name>.corrupt-<timestamp>.json`, and Aegis starts with an empty list.
- Deleting a project forgets it and its sessions. The workspace folder is never touched.

**Keys** go to the OS credential store under the service **Aegis** (Credential Manager, Keychain,
Secret Service), where you can inspect or delete them without Aegis. The default provider's key is
the account **provider-api-key**; every other provider's is `provider-api-key:<id>`, with the
id from `settings.json`. `AEGIS_API_KEY` in the environment fills the default provider only; the
credential store wins when both are set. A CLI login stays in that CLI's own file.

The optional TypeSafe key (*Settings → Decision model*) is a second credential, not a provider: the
account **typesafe-api-key**, or `AEGIS_TYPESAFE_API_KEY` in the environment. Its model, origin and
the *annotate approvals* toggle sit in `settings.json` under `decision`, beside `providers`; a save
of either half writes the other back unchanged.

A `settings.json` from before the roster (one `provider` object) is read as the default provider and
rewritten as a `providers` list on the next save.

Beside the documents:

- **`skills/`** — your runbook library, one directory per skill. Aegis seeds twenty-five runbooks
  (`never-send-without-review`, `cos.loop`, `cabinet.found`, the four `world.*` runbooks and the
  eighteen of the [domain packs](packs.md)) and records each name in `skills/.seeded`: a runbook you
  delete stays deleted, and a later version only offers names it has not offered before. See
  [Skills](skills.md).
- **`captures/`** — the PNGs `screen_capture` writes, named
  `capture-<UTC timestamp>-<random>.png`. They are kept out of your workspace so a capture never
  lands in a commit, and nothing deletes them. This is the only directory the window may read a file
  from, through the `asset:` protocol scoped to it at startup.
- **`audit.jsonl`** — one JSON line per tool call, allowed, refused or failed: the session, the
  identity, the skill run, the routine, the delegation, the tool, the policy's reason, the outcome,
  and the paths the call touched. Never file contents, and never tokens (those are counted per turn
  on the session). A capture's line has its path, pixel size and SHA-256. The file is append-only
  and never rotated; the **Audit log** drawer reads its last 200 lines and cannot write to it.

## Where commands run

By default `shell_exec` runs on this computer: the program is looked up on this process's PATH.

On Windows, a project can name a **WSL distribution** as its execution host instead (*Commands run
in* in the sidebar). The list comes from `wsl -l -q`, read each time a project opens.

- **Nothing is inferred.** Opening a folder under `\\wsl$\` does not set a host. The picker marks
  the distribution the folder is in, and stops there — `C:\work\proj` is just as likely to be built
  in WSL as a `\\wsl$\` path.
- A command runs as `wsl -d <distro> --cd <dir> --exec <program> <args>`: still no shell, and the
  arguments stay a vector. PATH, HOME and the user are the distribution's; nothing runs as root.
- The approval dialog names the distribution and the Linux working directory beside the Windows
  path. A session grant is still keyed on the program, never on `wsl.exe`. The model cannot call
  `wsl` itself.
- Stop and the deadline end the Linux process, not only the `wsl.exe` relay.
- The model is told which distribution it is in and the workspace path from inside it.
- **The file tools do not move.** Containment is still the Windows-canonical workspace, `fs_*`
  still go through Windows, and a capture is still this display.
- The `git init` behind *Set up shared files* uses the distribution's `git` too.
- **It never falls back.** A missing distribution, a WSL service that does not answer, or a folder
  the distribution cannot see fails with `E_EXEC_HOST` before anything runs. The working directory
  is probed first (about 150 ms), because `wsl --cd` silently starts in `/` when it cannot find the
  directory.

Sessions, routines and delegated runs inherit the project's host and cannot override it. This is not
a second Aegis, a VM manager, or a way to run Aegis under WSLg. Design: `PLAN.md` § 7.12.
