/**
 * Typed `invoke` wrappers — one function per Rust command.
 *
 * Components never call `invoke` directly: the command name and the argument
 * shape are spelled out exactly once, here, and every rejection is normalized
 * to an {@link IpcError}. Payload *types* move the other way — they are
 * generated into `./bindings.ts` from the Rust structs by `ts-rs`, so this
 * file imports them rather than restating them.
 *
 * Commands land with their phases: the window and application lifecycle
 * (PLAN 2.1, "Window / tray"), projects (PLAN 2.1, "Projects"), sessions and
 * turns (PLAN 2.1, "Sessions and turns"), approvals (PLAN 2.1, "Approvals"),
 * the audit log and the provider settings (PLAN 2.1, "Settings and audit"),
 * the shared-workspace convention (PLAN 7.3, Phase 11), the identities a
 * session can be opened as (PLAN 7.3, Phase 12), the runbooks those
 * identities may run (PLAN 7.3, Phase 13), what one identity has learned
 * (PLAN 7.3, Phase 14), the routines that fire a runbook on a clock
 * (PLAN 7.3, Phase 16), and the project's board and the runs its audit log
 * folds into (PLAN 7.3, Phase 17).
 *
 * Argument keys are `snake_case`, matching the Rust parameter names — the
 * commands are declared `rename_all = "snake_case"`, so the camelCase Tauri
 * would otherwise accept is deliberately not part of the contract.
 */

import { invoke } from "@tauri-apps/api/core";
import type { InvokeArgs } from "@tauri-apps/api/core";

import { toIpcError } from "../lib/errors";
import type {
  Agent,
  AgentDraft,
  ApprovalRequest,
  AuditEntry,
  AuthKind,
  Board,
  Decision,
  Grant,
  MaskedSettings,
  ModelCatalog,
  Memory,
  MemoryDraft,
  Project,
  ProjectDetail,
  ProviderProbe,
  Routine,
  RoutineDraft,
  RunRef,
  RunTrace,
  ScaffoldReport,
  SessionDetail,
  SessionSummary,
  Skill,
  TurnHandle,
  WorkspaceLayout,
} from "./bindings";

async function call<T>(command: string, args?: InvokeArgs): Promise<T> {
  try {
    return args === undefined
      ? await invoke<T>(command)
      : await invoke<T>(command, args);
  } catch (cause) {
    throw toIpcError(cause, command);
  }
}

/**
 * Shows the main window if it is hidden or unfocused, hides it otherwise —
 * the same toggle the tray icon performs.
 */
export function windowToggle(): Promise<void> {
  return call<void>("window_toggle");
}

/** Hides the main window to the tray. The process keeps running. */
export function windowHide(): Promise<void> {
  return call<void>("window_hide");
}

/**
 * Whether the runtime installed a tray icon. False under WSL, or when
 * AppIndicator is missing: *Hide to tray* would then leave no way back.
 */
export function windowHasTray(): Promise<boolean> {
  return call<boolean>("window_has_tray");
}

/**
 * Quits Aegis.
 *
 * Resolves only if the runtime rejects the request; a successful quit tears
 * the WebView down before the promise settles, so callers must not rely on
 * anything running after this.
 */
export function appQuit(): Promise<void> {
  return call<void>("app_quit");
}

/**
 * Opens the native folder picker and returns the chosen workspace path,
 * already canonicalized.
 *
 * Resolves to `null` when the user cancels. A cancelled dialog is an ordinary
 * outcome, not an error — callers should do nothing rather than report it.
 *
 * The dialog itself runs in Rust: the WebView holds no `dialog:` permission,
 * so this command is the only way to open one.
 */
export function projectPickWorkspace(): Promise<string | null> {
  return call<string | null>("project_pick_workspace");
}

/**
 * Registers a workspace folder as a project.
 *
 * An empty `name` falls back to the folder's own name. Adding a folder that is
 * already a project returns that existing project instead of a duplicate, so
 * this is safe to call again after a re-pick.
 */
export function projectCreate(name: string, path: string): Promise<Project> {
  return call<Project>("project_create", { name, path });
}

/** Every project, most recently opened first. */
export function projectList(): Promise<Project[]> {
  return call<Project[]>("project_list");
}

/**
 * Opens a project, marking it the most recent one.
 *
 * Rejects with `E_INTERNAL` if the project is gone — the list the caller is
 * holding is stale, and the fix is to refetch rather than to branch on the
 * code.
 */
export function projectOpen(projectId: string): Promise<ProjectDetail> {
  return call<ProjectDetail>("project_open", { project_id: projectId });
}

/** Forgets a project. The workspace folder on disk is never touched. */
export function projectDelete(projectId: string): Promise<void> {
  return call<void>("project_delete", { project_id: projectId });
}
/**
 * Every identity: the built-in one first, then the rest by name.
 *
 * The built-in one carries `builtin: true`, no role and no instructions. It is
 * what a session gets when none is chosen, and it cannot be edited or deleted —
 * it is the assistant Aegis had before identities existed, written down.
 */
export function agentList(): Promise<Agent[]> {
  return call<Agent[]>("agent_list");
}

/**
 * Creates an identity.
 *
 * Rejects with `E_INVALID_SETTING` when a value cannot be used. `error.field`
 * names which input — "name", "role", "instructions", "provider", "tools",
 * "skills" — and `error.message` says what a working value looks like, so the
 * form marks the input rather than raising a banner over itself.
 */
export function agentCreate(draft: AgentDraft): Promise<Agent> {
  return call<Agent>("agent_create", { draft });
}

/**
 * Replaces an identity's fields, keeping its id.
 *
 * The sessions already bound to it stay bound and pick the change up on their
 * next turn: correcting what a "reviewer" is should reach the reviewers.
 * Rejects for the built-in identity.
 */
export function agentUpdate(agentId: string, draft: AgentDraft): Promise<Agent> {
  return call<Agent>("agent_update", { agent_id: agentId, draft });
}

/**
 * Deletes an identity.
 *
 * Rejects while any session still runs as it — the message says how many —
 * rather than moving those sessions to another identity, which would rewrite
 * what they were. Delete the sessions first, or keep it.
 */
export function agentDelete(agentId: string): Promise<void> {
  return call<void>("agent_delete", { agent_id: agentId });
}

/**
 * Creates a session in a project, as an identity.
 *
 * An omitted title becomes "New session", which the first message the user
 * sends then replaces with its own opening words. An omitted `agentId` is the
 * built-in identity.
 *
 * The identity is fixed here. There is deliberately no command that rebinds a
 * session: a transcript is the record of what one identity did, and moving it
 * under another would leave calls in the history of an identity that was never
 * allowed to make them. Working as someone else is a new session.
 */
export function sessionCreate(
  projectId: string,
  title?: string,
  agentId?: string,
): Promise<SessionSummary> {
  return call<SessionSummary>("session_create", {
    project_id: projectId,
    title: title ?? null,
    agent_id: agentId ?? null,
  });
}

/** A project's sessions, most recently active first. */
export function sessionList(projectId: string): Promise<SessionSummary[]> {
  return call<SessionSummary[]>("session_list", { project_id: projectId });
}

/**
 * Opens a session and returns its transcript.
 *
 * `session.state` is live rather than stored: a session is `running` only if a
 * turn is in flight in this process right now, so a restart never reports one
 * that nothing is left to finish.
 */
export function sessionOpen(sessionId: string): Promise<SessionDetail> {
  return call<SessionDetail>("session_open", { session_id: sessionId });
}

/** Renames a session. An empty title is rejected by the runtime. */
export function sessionRename(sessionId: string, title: string): Promise<void> {
  return call<void>("session_rename", { session_id: sessionId, title });
}

/** Deletes a session and its transcript. A running turn is cancelled first. */
export function sessionDelete(sessionId: string): Promise<void> {
  return call<void>("session_delete", { session_id: sessionId });
}

/**
 * Sends a message and starts a turn.
 *
 * Resolves as soon as the turn is registered — not when the reply is done.
 * Everything after that arrives as `turn:*` and `tool:*` events; the returned
 * handle is what {@link sessionCancel} needs.
 *
 * Rejects with `E_TURN_BUSY` when the session is already running one. That is
 * worth branching on: the right response is to keep the text in the composer
 * and try again when the running turn finishes, not to report a failure.
 */
export function sessionSend(
  sessionId: string,
  text: string,
): Promise<TurnHandle> {
  return call<TurnHandle>("session_send", { session_id: sessionId, text });
}

/**
 * Cancels a running turn.
 *
 * Whatever the model had already streamed is kept in the transcript: the user
 * saw it, and a transcript that disagrees with what was on screen is worse
 * than a short one. Rejects when the handle is from a turn that has already
 * finished, which is the UI's cue to refetch.
 */
export function sessionCancel(
  sessionId: string,
  turnId: string,
): Promise<void> {
  return call<void>("session_cancel", {
    session_id: sessionId,
    turn_id: turnId,
  });
}

/**
 * What a session is currently blocked on, oldest first.
 *
 * A re-sync, not the primary path: approvals normally arrive as
 * `tool:approval_required` events. This is what a window calls after a reload,
 * or when switching to a session whose turn was already waiting — the runtime
 * is authoritative, and a card the runtime does not list here can no longer be
 * answered whatever is still on screen.
 *
 * Omitting `sessionId` returns every session's queue.
 */
export function approvalListPending(
  sessionId?: string,
): Promise<ApprovalRequest[]> {
  return call<ApprovalRequest[]>("approval_list_pending", {
    session_id: sessionId ?? null,
  });
}

/**
 * Answers one approval, releasing the turn that is parked on it.
 *
 * Resolving is all this does. Whether the call then runs is the turn loop's
 * business, and the result arrives as `tool:started` / `tool:finished` — there
 * is deliberately no command that executes a tool, because that would be a
 * second way into the machine that policy does not gate.
 *
 * Two rejections are worth branching on:
 *
 * - `E_APPROVAL_STALE` — the request expired, was already answered, or its
 *   turn was cancelled. Re-sync with {@link approvalListPending}; the click did
 *   nothing, and telling the user it succeeded would be a lie.
 * - `E_GRANT_NOT_ALLOWED` — `allow_session` on a row that offers no grant. The
 *   UI does not draw that button when `session_grant_allowed` is false, so this
 *   is the runtime refusing to trust the WebView rather than a path a user
 *   reaches by clicking. The request stays open.
 */
export function approvalResolve(
  requestId: string,
  decision: Decision,
): Promise<void> {
  return call<void>("approval_resolve", {
    request_id: requestId,
    decision,
  });
}

/**
 * The `allow_session` grants a session currently holds.
 *
 * Empty for a session that has only ever answered "allow once" — which is the
 * point of that answer. Grants never outlive the process (PLAN 3.1), so this
 * is always empty for a session opened after a restart.
 */
export function approvalGrants(sessionId: string): Promise<Grant[]> {
  return call<Grant[]>("approval_grants", { session_id: sessionId });
}

/**
 * Withdraws one grant, so the tool it covered is asked about again.
 *
 * Idempotent: revoking something already revoked resolves to `false` rather
 * than rejecting. The user's intent is satisfied either way.
 */
export function approvalRevokeGrant(
  sessionId: string,
  grant: Grant,
): Promise<boolean> {
  return call<boolean>("approval_revoke_grant", {
    session_id: sessionId,
    grant,
  });
}

/**
 * The most recent audit entries, newest first.
 *
 * One entry per tool call — allowed, refused or failed. `sessionId` narrows it
 * to one session's calls; `limit` defaults to 100 in Rust and is clamped to
 * 1000 there, so asking for more is not an error, it simply returns 1000.
 *
 * There is no counterpart that writes or clears the log. Entries are produced
 * by the runtime as a side effect of running a tool, and a UI that could
 * append to the log — or empty it — would be a UI that could forge or erase
 * the record of what the agent did.
 */
export function auditTail(
  limit?: number,
  sessionId?: string,
): Promise<AuditEntry[]> {
  return call<AuditEntry[]>("audit_tail", {
    limit: limit ?? null,
    session_id: sessionId ?? null,
  });
}

/**
 * Where the audit log lives on disk.
 *
 * Returned even before anything has been written: the first tool call creates
 * the file, and telling the user where it will be is more useful than an
 * error. The file is plain JSONL and is meant to be readable without Aegis.
 */
export function auditLogPath(): Promise<string> {
  return call<string>("audit_log_path");
}

/**
 * The open project's board: attention, in flight, blocked, and its runs.
 *
 * Measured on every call. Half of what it returns is live — what is running,
 * what a dialog is waiting on, which clock stopped itself — so there is
 * deliberately nothing to cache and no `board:changed` event: the panel asks
 * again when something it already listens for says the answer may have moved.
 *
 * There is no counterpart that writes one. The file half is the user's own
 * `status/STATUS.md`, and the way it is corrected is an ordinary `fs_write`
 * through the approval gate, exactly like a decision.
 */
export function boardRead(projectId: string): Promise<Board> {
  return call<Board>("board_read", { project_id: projectId });
}

/**
 * One run, and the audit lines it is replayed from, oldest first.
 *
 * The reference is handed back from the board unchanged — it names a grouping
 * of lines already in the log, not a path or anything else the window could
 * widen. A run the panel is holding but the log has scrolled past rejects, and
 * refetching the board is the answer.
 */
export function boardTrace(
  projectId: string,
  run: RunRef,
): Promise<RunTrace> {
  return call<RunTrace>("board_trace", { project_id: projectId, run });
}

/**
 * The provider settings, with the key masked.
 *
 * `key_hint` is a few characters for recognition and `key_source` says which
 * store answered. There is deliberately no command that returns the key
 * itself: it lives in the OS credential store or in the environment, and the
 * WebView is never given it (`AGENTS.md`).
 */
export function settingsGet(): Promise<MaskedSettings> {
  return call<MaskedSettings>("settings_get");
}

/**
 * Saves the base URL and the model, and the key when one is passed.
 *
 * Omit `apiKey` — or pass an empty string — to leave the stored key alone,
 * which is the ordinary case: changing a model does not mean retyping a
 * credential. Removing one is {@link settingsClearKey}.
 *
 * Two rejections are worth branching on:
 *
 * - `E_INVALID_SETTING` — the value cannot be used. `error.field` names which
 *   input, and `error.message` says what a working one looks like. Nothing was
 *   saved, including the key, so the form stays open with the text in it.
 * - `E_KEYRING_UNAVAILABLE` — this machine has no usable credential store. The
 *   base URL and the model *were* saved; only the key was not, and the answer
 *   is to set `AEGIS_API_KEY` in the environment instead.
 */
export function settingsSet(
  baseUrl: string,
  model: string,
  apiKey?: string,
  authKind?: AuthKind,
): Promise<MaskedSettings> {
  return call<MaskedSettings>("settings_set", {
    base_url: baseUrl,
    model,
    api_key: apiKey ?? null,
    auth_kind: authKind ?? "api_key",
  });
}

/**
 * Lists the models the chosen authentication can use.
 *
 * `baseUrl` is the one currently in the form, which may not have been saved.
 * A live list is preferred; if the server cannot be asked the payload still
 * carries a fallback and a sentence saying why.
 */
export function settingsListModels(
  authKind: AuthKind,
  baseUrl: string,
): Promise<ModelCatalog> {
  return call<ModelCatalog>("settings_list_models", {
    auth_kind: authKind,
    base_url: baseUrl,
  });
}

/**
 * Removes the key from the OS credential store.
 *
 * Only that one. A key in `AEGIS_API_KEY` belongs to the environment Aegis was
 * started in and is not Aegis's to edit, so the settings this returns may
 * still report `key_source: "env"` — which is the honest answer, and what the
 * panel says out loud rather than leaving the user to wonder why the key came
 * back.
 */
export function settingsClearKey(): Promise<MaskedSettings> {
  return call<MaskedSettings>("settings_clear_key");
}

/**
 * Asks the configured server whether it is reachable, the key works and the
 * model exists.
 *
 * Sends one very short completion to the same endpoint a turn would use, so it
 * costs a few tokens. That is deliberate: a probe of some *other* endpoint can
 * fail on a configuration where chat works perfectly, and a test that cries
 * wolf is one people stop reading.
 *
 * Never rejects. Every outcome — unreachable, rejected key, no endpoint,
 * unknown model, nothing configured at all — comes back as a
 * {@link ProviderProbe} whose `message` says what happened, because "the probe
 * failed" is not useful to someone who pressed a button to find out what is
 * wrong.
 */
export function settingsProbeProvider(): Promise<ProviderProbe> {
  return call<ProviderProbe>("settings_probe_provider");
}

/**
 * Which parts of the shared-workspace convention exist in a project's folder.
 *
 * Measured at every call rather than cached. The folder belongs to the user,
 * who may well have made `decisions/` in a terminal a minute ago, and a panel
 * that is confidently wrong about someone's own directory is worse than no
 * panel. A project whose folder is missing reports every entry absent, which is
 * the truth rather than a failure.
 */
export function workspaceLayout(projectId: string): Promise<WorkspaceLayout> {
  return call<WorkspaceLayout>("workspace_layout", { project_id: projectId });
}

/**
 * Creates the missing directories and seed files, and nothing else.
 *
 * Never overwrites: a file that is already there comes back under `kept`,
 * byte for byte as it was. Safe to call again — a second run creates nothing
 * and returns the same report.
 *
 * There is deliberately no command for *editing* those files. Writing a
 * decision or a status is `fs_write`, which goes through the approval dialog
 * and onto the audit log like every other change to the workspace.
 */
export function workspaceScaffold(projectId: string): Promise<ScaffoldReport> {
  return call<ScaffoldReport>("workspace_scaffold", { project_id: projectId });
}

/**
 * Every runbook the skill library and one project's workspace hold.
 *
 * `projectId` may be `null`: Settings is reachable with nothing open, and the
 * library alone is the honest answer when there is no workspace to look in.
 * A project whose folder has gone lists the library alone rather than failing.
 *
 * Measured on every call rather than cached, like the workspace layout: a
 * `SKILL.md` is an ordinary file in a folder the user owns, and one edited in
 * their editor a minute ago is the one the next turn will run.
 *
 * There is deliberately no command that *writes* a runbook, and none that runs
 * one. Writing is an editor's job, or an ordinary `fs_write` under the gate;
 * running is something a turn does, under the identity's allow-list.
 */
export function skillList(projectId: string | null): Promise<Skill[]> {
  return call<Skill[]>("skill_list", { project_id: projectId });
}

/**
 * One identity's memories, most recently touched first.
 *
 * `agentId` is required rather than defaulted: "whose memory" is the whole
 * question this panel answers, and a list that quietly showed the built-in
 * identity's while a picker was still loading would be the one wrong thing it
 * could draw.
 */
export function memoryList(agentId: string): Promise<Memory[]> {
  return call<Memory[]>("memory_list", { agent_id: agentId });
}

/**
 * Records a memory, or corrects one.
 *
 * `memoryId` of `null` records a new one; otherwise it replaces that one,
 * keeping its id. A refused field rejects with `E_INVALID_SETTING` and an
 * `error.field` naming the input — the same shape the provider and identity
 * forms already use.
 *
 * A new memory whose text an existing one already carries touches that one
 * rather than storing a second copy, and resolves with the existing id. A
 * caller that assumed it had created a row will find it already in the list,
 * which is the truth.
 */
export function memorySave(
  agentId: string,
  memoryId: string | null,
  draft: MemoryDraft,
): Promise<Memory> {
  return call<Memory>("memory_save", {
    agent_id: agentId,
    memory_id: memoryId,
    draft,
  });
}

/**
 * Forgets one memory, and resolves with what went.
 *
 * There is no undo — the store is the only copy — which is why the panel
 * confirms first and shows what it removed.
 */
export function memoryForget(
  agentId: string,
  memoryId: string,
): Promise<Memory> {
  return call<Memory>("memory_forget", {
    agent_id: agentId,
    memory_id: memoryId,
  });
}

/**
 * Folds this session's older turns into state (PLAN 7.3, Phase 14).
 *
 * The button behind the fold every turn already does on its own once a
 * transcript has grown expensive. Resolves with the session as it now reads,
 * whether or not anything moved: a session with too few turns to fold is an
 * answer, not a failure, and the caller sees it by finding no `compaction` on
 * what comes back.
 *
 * Rejects with `E_TURN_BUSY` while a turn is running: a turn folds once, before
 * its first request, so that all of its rounds reason against the same history.
 *
 * Nothing is deleted. The transcript stays on disk in full and the pane still
 * scrolls through all of it; what changes is only what reaches the model.
 */
export function sessionCompact(sessionId: string): Promise<SessionDetail> {
  return call<SessionDetail>("session_compact", { session_id: sessionId });
}

/**
 * Every routine, each carrying whatever is wrong with it right now
 * (PLAN 7.3, Phase 16).
 *
 * `problem` is measured on every call, never stored: a skill that was
 * un-granted, a folder that was unplugged, a budget that is spent. A row that
 * carries one is a row that will not fire, and the panel says so rather than
 * drawing a clock that has quietly stopped.
 */
export function routineList(): Promise<Routine[]> {
  return call<Routine[]>("routine_list");
}

/**
 * Creates a routine, or replaces one.
 *
 * `routineId` of `null` creates; otherwise that routine is updated, keeping its
 * id and today's ledger.
 *
 * The runtime enforces the door here: the skill has to be live, granted to the
 * identity, and already run under watch at least once — evidence it reads from
 * the audit log. A refusal rejects with `E_INVALID_SETTING` and an
 * `error.field` naming the input, the same shape the identity and provider
 * forms use.
 */
export function routineSave(
  routineId: string | null,
  draft: RoutineDraft,
): Promise<Routine> {
  return call<Routine>("routine_save", {
    routine_id: routineId,
    draft,
  });
}

/**
 * Deletes a routine. The sessions its runs opened are left alone — they are
 * transcripts of things that happened.
 */
export function routineDelete(routineId: string): Promise<void> {
  return call<void>("routine_delete", { routine_id: routineId });
}

/**
 * Stops or restarts a routine's clock.
 *
 * Un-pausing re-arms it, so a routine stopped for a fortnight does not
 * immediately fire for a window nobody was there for, and it clears the reason
 * — including one the scheduler wrote itself after two runs that never
 * reported.
 */
export function routineSetPaused(
  routineId: string,
  paused: boolean,
): Promise<Routine> {
  return call<Routine>("routine_set_paused", {
    routine_id: routineId,
    paused,
  });
}

/**
 * Fires a routine now, exactly as the clock would.
 *
 * Resolves as soon as the run has started; the session, the row and the
 * transcript arrive as `session:updated`, `routine:updated` and `turn:*`
 * events. The run is unattended like any other, so a call the routine was not
 * signed for is refused here just as it would be at four in the morning —
 * which is the point of pressing it.
 */
export function routineRunNow(routineId: string): Promise<void> {
  return call<void>("routine_run_now", { routine_id: routineId });
}
