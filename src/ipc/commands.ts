/**
 * Typed `invoke` wrappers — one function per Rust command.
 *
 * Components never call `invoke` directly; every rejection becomes an
 * {@link IpcError}. Payload types are generated into `./bindings.ts`. The
 * command list is PLAN 2.1.
 *
 * Argument keys are `snake_case`, matching the Rust parameters
 * (`rename_all = "snake_case"`).
 */

import { invoke } from "@tauri-apps/api/core";
import type { InvokeArgs } from "@tauri-apps/api/core";

import { toIpcError } from "../lib/errors";
import type {
  Agent,
  AgentDraft,
  ApprovalRequest,
  AttachReport,
  AuditEntry,
  AuthKind,
  Board,
  ConnectorDraft,
  ConnectorView,
  Decision,
  ExecHost,
  ExecHostOption,
  FilePreview,
  Grant,
  ImportReport,
  MaskedSettings,
  EvalEntry,
  EvalProposal,
  ModelCatalog,
  Memory,
  MemoryDraft,
  ParkedAsk,
  Project,
  ProjectDetail,
  ProviderProbe,
  RosterApplied,
  RosterProposal,
  Routine,
  RoutineDraft,
  RunRef,
  RunTrace,
  ScaffoldReport,
  SessionDetail,
  SessionSummary,
  Skill,
  SkillProposal,
  TreeListing,
  TurnHandle,
  WorkspaceLayout,
  WorldStatus,
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
 * Opens an `http`/`https` link in the OS browser (PLAN 7.20). Anything else is
 * refused (`E_PATH_INVALID`); the window itself never navigates.
 */
export function openUrl(url: string): Promise<void> {
  return call<void>("open_url", { url });
}

/** Quits Aegis. On success the WebView is torn down before this settles. */
export function appQuit(): Promise<void> {
  return call<void>("app_quit");
}

/**
 * Opens the native folder picker (from Rust; the WebView has no `dialog:`
 * permission). Resolves to the canonical path, or `null` if cancelled.
 */
export function projectPickWorkspace(): Promise<string | null> {
  return call<string | null>("project_pick_workspace");
}

/**
 * Registers a workspace folder as a project. An empty `name` uses the folder's
 * name; a folder already registered returns the existing project.
 */
export function projectCreate(name: string, path: string): Promise<Project> {
  return call<Project>("project_create", { name, path });
}

/** Every project, most recently opened first. */
export function projectList(): Promise<Project[]> {
  return call<Project[]>("project_list");
}

/**
 * Opens a project, marking it the most recent. Rejects with `E_INTERNAL` if it
 * is gone: refetch the list.
 */
export function projectOpen(projectId: string): Promise<ProjectDetail> {
  return call<ProjectDetail>("project_open", { project_id: projectId });
}

/** Forgets a project. The workspace folder on disk is never touched. */
export function projectDelete(projectId: string): Promise<void> {
  return call<void>("project_delete", { project_id: projectId });
}

/**
 * Where a project's commands could run (PLAN 7.12): this computer, then the
 * WSL distributions `wsl.exe -l -q` lists.
 */
export function projectListExecHosts(): Promise<ExecHostOption[]> {
  return call<ExecHostOption[]>("project_list_exec_hosts");
}

/**
 * Sets where a project's commands run; `null` is this computer. Rejects with
 * `E_EXEC_HOST` when the distribution is missing, the build is not Windows, or
 * the folder has no path in that distribution.
 */
export function projectSetExecHost(
  projectId: string,
  host: ExecHost | null,
): Promise<Project> {
  return call<Project>("project_set_exec_host", {
    project_id: projectId,
    host,
  });
}

/**
 * Every identity: the built-in one (`builtin: true`, not editable) first, then
 * the rest by name.
 */
export function agentList(): Promise<Agent[]> {
  return call<Agent[]>("agent_list");
}

/**
 * Creates an identity. A refused value rejects with `E_INVALID_SETTING` and
 * `error.field` naming the input.
 */
export function agentCreate(draft: AgentDraft): Promise<Agent> {
  return call<Agent>("agent_create", { draft });
}

/**
 * Replaces an identity's fields, keeping its id; bound sessions pick the change
 * up next turn. Rejects for the built-in identity.
 */
export function agentUpdate(agentId: string, draft: AgentDraft): Promise<Agent> {
  return call<Agent>("agent_update", { agent_id: agentId, draft });
}

/**
 * Deletes an identity. Rejects while sessions or routines still use it; they
 * are never reassigned.
 */
export function agentDelete(agentId: string): Promise<void> {
  return call<void>("agent_delete", { agent_id: agentId });
}

/**
 * Creates a session in a project, bound for good to `agentId` (default: the
 * built-in identity). Without a title, the first message names it. An omitted
 * `providerId` or `model` inherits the identity's (PLAN 7.19).
 */
export function sessionCreate(
  projectId: string,
  title?: string,
  agentId?: string,
  providerId?: string,
  model?: string,
): Promise<SessionSummary> {
  return call<SessionSummary>("session_create", {
    project_id: projectId,
    title: title ?? null,
    agent_id: agentId ?? null,
    provider_id: providerId ?? null,
    model: model ?? null,
  });
}

/**
 * Overrides the provider row and model a session answers from (PLAN 7.19).
 * Both `null` returns to the identity's pair; the identity never changes.
 * `E_TURN_BUSY` while a turn runs.
 */
export function sessionSetBinding(
  sessionId: string,
  providerId: string | null,
  model: string | null,
): Promise<SessionSummary> {
  return call<SessionSummary>("session_set_binding", {
    session_id: sessionId,
    provider_id: providerId,
    model,
  });
}

/** A project's sessions, most recently active first. */
export function sessionList(projectId: string): Promise<SessionSummary[]> {
  return call<SessionSummary[]>("session_list", { project_id: projectId });
}

/** Opens a session: its transcript, and its live (never stored) state. */
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
 * Sends a message and starts a turn. Resolves once the turn is registered; the
 * rest arrives as `turn:*` and `tool:*` events. `E_TURN_BUSY` means keep the
 * text and wait.
 */
export function sessionSend(
  sessionId: string,
  text: string,
  attachments: readonly string[] = [],
): Promise<TurnHandle> {
  return call<TurnHandle>("session_send", {
    session_id: sessionId,
    text,
    attachments: attachments.length === 0 ? null : attachments,
  });
}

/**
 * Opens the image picker (in Rust; the window has no `dialog:` permission) and
 * copies what was chosen under the app's data (PLAN 7.20). Cancelled is an
 * empty report. The ids go back with `sessionSend`.
 */
export function attachmentPick(): Promise<AttachReport> {
  return call<AttachReport>("attachment_pick");
}

/**
 * Copies the images of a drop onto the composer. `dropId` comes from
 * `workspace:dropped`; such a drop never becomes a brief.
 */
export function attachmentDrop(dropId: string): Promise<AttachReport> {
  return call<AttachReport>("attachment_drop", { drop_id: dropId });
}

/**
 * Cancels a running turn, keeping what already streamed. Rejects for a turn
 * that already finished: refetch.
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
 * Pending approvals, oldest first — a re-sync after a reload; normally they
 * arrive as `tool:approval_required`. Omit `sessionId` for every session.
 */
export function approvalListPending(
  sessionId?: string,
): Promise<ApprovalRequest[]> {
  return call<ApprovalRequest[]>("approval_list_pending", {
    session_id: sessionId ?? null,
  });
}

/**
 * Answers one approval, releasing its turn. No command runs a tool directly.
 *
 * - `E_APPROVAL_STALE`: expired, answered or cancelled — re-sync with
 *   {@link approvalListPending}.
 * - `E_GRANT_NOT_ALLOWED`: `allow_session` on a row without a grant; the
 *   request stays open.
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

/** The session grants a session holds; they never outlive the process (PLAN 3.1). */
export function approvalGrants(sessionId: string): Promise<Grant[]> {
  return call<Grant[]>("approval_grants", { session_id: sessionId });
}

/** Withdraws one grant. Idempotent: `false` if it was already gone. */
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
 * The most recent audit entries, newest first, one per tool call. `limit`
 * defaults to 100 and is clamped to 1000. The WebView can never write the log.
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

/** Where the JSONL audit log lives, even before the first call creates it. */
export function auditLogPath(): Promise<string> {
  return call<string>("audit_log_path");
}

/**
 * The open project's board: attention, in flight, blocked, and runs. Measured
 * on every call, with no `board:changed` event. `STATUS.md` is edited through
 * `fs_write`, never here.
 */
export function boardRead(projectId: string): Promise<Board> {
  return call<Board>("board_read", { project_id: projectId });
}

/**
 * One run and its audit lines, oldest first. `run` comes from the board as-is;
 * a run the log has scrolled past rejects — refetch the board.
 */
export function boardTrace(
  projectId: string,
  run: RunRef,
): Promise<RunTrace> {
  return call<RunTrace>("board_trace", { project_id: projectId, run });
}

/**
 * The calls this project's runs parked for a person, oldest first (PLAN 7.22).
 * Omitting the project asks for every one of them.
 */
export function parkedList(projectId?: string): Promise<ParkedAsk[]> {
  return call<ParkedAsk[]>("parked_list", { project_id: projectId ?? null });
}

/**
 * Answers one parked ask and picks its run up where it stopped.
 *
 * `allow_once` covers that exact call, `allow_session` signs a standing
 * approval onto the routine, `deny` records the refusal — and all three resume
 * the run. Rejects with `E_APPROVAL_STALE` when it was answered already or has
 * expired (refetch), `E_GRANT_NOT_ALLOWED` when no standing approval is on
 * offer, and `E_INVALID_SETTING` or `E_TURN_BUSY` when the run cannot be picked
 * up right now.
 */
export function parkedAnswer(
  parkedId: string,
  decision: Decision,
): Promise<void> {
  return call<void>("parked_answer", {
    parked_id: parkedId,
    decision,
  });
}

/**
 * Every provider row with its key masked (`key_hint`, `key_source`). No
 * command returns a key itself.
 */
export function settingsGet(): Promise<MaskedSettings> {
  return call<MaskedSettings>("settings_get");
}

/** What a save or an add sends for one provider row. */
export type ProviderRowInput = {
  readonly label: string;
  readonly baseUrl: string;
  readonly model: string;
  readonly authKind: AuthKind;
  /** Empty keeps the stored key (see {@link settingsClearKey}). */
  readonly apiKey: string;
};

/**
 * Saves one row, and its key if one is passed.
 *
 * - `E_INVALID_SETTING`: nothing was saved; `error.field` names the input.
 * - `E_KEYRING_UNAVAILABLE`: the row was saved, the key was not — use
 *   `AEGIS_API_KEY` (default row only).
 */
export function settingsSet(
  providerId: string,
  row: ProviderRowInput,
): Promise<MaskedSettings> {
  return call<MaskedSettings>("settings_set", {
    provider_id: providerId,
    label: row.label,
    base_url: row.baseUrl,
    model: row.model,
    api_key: row.apiKey.length === 0 ? null : row.apiKey,
    auth_kind: row.authKind,
  });
}

/** Appends a provider row under a fresh id, and its key if one is passed. */
export function settingsAddProvider(
  row: ProviderRowInput,
): Promise<MaskedSettings> {
  return call<MaskedSettings>("settings_add_provider", {
    label: row.label,
    base_url: row.baseUrl,
    model: row.model,
    api_key: row.apiKey.length === 0 ? null : row.apiKey,
    auth_kind: row.authKind,
  });
}

/**
 * Deletes a provider row. Refused for the default row and while an identity or
 * a session override names it.
 */
export function settingsDeleteProvider(
  providerId: string,
): Promise<MaskedSettings> {
  return call<MaskedSettings>("settings_delete_provider", {
    provider_id: providerId,
  });
}

/**
 * The models an authentication kind can use at the (possibly unsaved)
 * `baseUrl`: live if possible, else a fallback with the reason.
 */
export function settingsListModels(
  authKind: AuthKind,
  baseUrl: string,
  providerId?: string,
): Promise<ModelCatalog> {
  return call<ModelCatalog>("settings_list_models", {
    auth_kind: authKind,
    base_url: baseUrl,
    provider_id: providerId ?? null,
  });
}

/**
 * Removes one row's key from the OS credential store. A key in
 * `AEGIS_API_KEY` stays, so the default row may still say `key_source: "env"`.
 */
export function settingsClearKey(providerId: string): Promise<MaskedSettings> {
  return call<MaskedSettings>("settings_clear_key", {
    provider_id: providerId,
  });
}

/**
 * Checks the server is reachable, the key works and the model exists, with one
 * tiny completion on the turn's own endpoint (a few tokens). Never rejects:
 * every outcome is a {@link ProviderProbe} message.
 */
export function settingsProbeProvider(
  providerId: string,
): Promise<ProviderProbe> {
  return call<ProviderProbe>("settings_probe_provider", {
    provider_id: providerId,
  });
}

/** What the decision model's form saves (PLAN 7.18). */
export type DecisionInput = {
  /** Empty means `jev-latest`. */
  readonly model: string;
  /** An origin; empty means `https://api.typesafe.ai`. */
  readonly baseUrl: string;
  readonly annotateApprovals: boolean;
  /** Empty keeps the stored key. */
  readonly apiKey: string;
};

/**
 * Saves the decision model's settings, and its key if one is passed. Never
 * touches a provider row.
 */
export function settingsSetDecision(
  input: DecisionInput,
): Promise<MaskedSettings> {
  return call<MaskedSettings>("settings_set_decision", {
    model: input.model,
    base_url: input.baseUrl,
    annotate_approvals: input.annotateApprovals,
    api_key: input.apiKey.length === 0 ? null : input.apiKey,
  });
}

/** Removes the stored TypeSafe key. `AEGIS_TYPESAFE_API_KEY` stays. */
export function settingsClearDecisionKey(): Promise<MaskedSettings> {
  return call<MaskedSettings>("settings_clear_decision_key");
}

/** One cheap question to TypeSafe. Never rejects. */
export function settingsProbeDecision(): Promise<ProviderProbe> {
  return call<ProviderProbe>("settings_probe_decision");
}

/** Every signed `.aegis/evals/<name>/eval.yml` in a project's workspace. */
export function evalList(projectId: string): Promise<EvalEntry[]> {
  return call<EvalEntry[]>("eval_list", { project_id: projectId });
}

/**
 * Every `PROPOSAL.yml` in a project's workspace. Kept apart from
 * {@link evalList}: a proposal does not run until a person applies it.
 */
export function evalProposals(projectId: string): Promise<EvalProposal[]> {
  return call<EvalProposal[]>("eval_proposals", { project_id: projectId });
}

/**
 * Which parts of the shared-workspace convention exist, measured on every call.
 * A missing folder reports everything absent.
 */
export function workspaceLayout(projectId: string): Promise<WorkspaceLayout> {
  return call<WorkspaceLayout>("workspace_layout", { project_id: projectId });
}

/**
 * Creates the missing convention directories and seed files. Never overwrites
 * (existing files come back under `kept`); idempotent. Editing them is
 * `fs_write`.
 */
export function workspaceScaffold(projectId: string): Promise<ScaffoldReport> {
  return call<ScaffoldReport>("workspace_scaffold", { project_id: projectId });
}

/**
 * Opens a workspace path in the OS file manager (PLAN 7.10).
 *
 * Omit `path` to reveal the project folder — the title-bar button. A path
 * that is not inside that folder is refused; the WebView never opens
 * `file://` and never gains an opener permission.
 */
export function workspaceReveal(
  projectId: string,
  path?: string,
): Promise<void> {
  return call<void>("workspace_reveal", {
    project_id: projectId,
    path: path ?? null,
  });
}

/**
 * One folder of the open project's workspace (PLAN 7.15).
 *
 * `dir` omitted is the root. `ignored` lists `.git`, `node_modules` and what
 * the ignore files name, marked, instead of counting them in `hidden`. A path
 * outside the workspace rejects with `E_PATH_OUTSIDE_WORKSPACE`: this is not a
 * listing the window can aim anywhere.
 */
export function workspaceTree(
  projectId: string,
  dir?: string,
  ignored?: boolean,
): Promise<TreeListing> {
  return call<TreeListing>("workspace_tree", {
    project_id: projectId,
    dir: dir ?? null,
    ignored: ignored ?? null,
  });
}

/** One file of the open project: name, size, type, and text if it is text. Read-only. */
export function workspacePreview(
  projectId: string,
  path: string,
): Promise<FilePreview> {
  return call<FilePreview>("workspace_preview", {
    project_id: projectId,
    path,
  });
}

/**
 * One image's bytes, for a blob URL the window creates.
 *
 * Arrives as an `ArrayBuffer` — a binary response, not base64 in JSON. The
 * window never loads a workspace file through `file://` or `asset:`.
 */
export function workspaceImage(
  projectId: string,
  path: string,
): Promise<ArrayBuffer> {
  return call<ArrayBuffer>("workspace_image", { project_id: projectId, path });
}

/**
 * Copies one drop's files into `.aegis/briefs/`. `dropId` comes from
 * `workspace:dropped`; the window never sends a path. Never moves, overwrites or
 * creates `briefs/` (`E_PATH_INVALID`, drop kept briefly).
 */
export function workspaceImportBrief(
  projectId: string,
  dropId: string,
): Promise<ImportReport> {
  return call<ImportReport>("workspace_import_brief", {
    project_id: projectId,
    drop_id: dropId,
  });
}

/**
 * Whether this project's folder holds a world, and its status — including
 * reading declared sources to report drift. Read-only; there is no world
 * scaffold (a world starts with `world/essence.md`).
 */
export function worldStatus(projectId: string): Promise<WorldStatus> {
  return call<WorldStatus>("world_status", { project_id: projectId });
}

/**
 * Every runbook in the library and, if `projectId` has a folder, its workspace.
 * Measured on every call. No command writes or runs one.
 */
export function skillList(projectId: string | null): Promise<Skill[]> {
  return call<Skill[]>("skill_list", { project_id: projectId });
}

/**
 * Every `PROPOSAL.md` waiting in one project's workspace (PLAN 7.13).
 *
 * Empty for `null` and for a folder that has gone: the library holds no
 * proposals. Listing only — applying one is a session's `fs_write`, signed in
 * its approval dialog, and there is no command for it.
 */
export function skillProposals(
  projectId: string | null,
): Promise<SkillProposal[]> {
  return call<SkillProposal[]>("skill_proposals", { project_id: projectId });
}

/**
 * The open project's roster proposal, judged against the identities on file
 * (PLAN 7.14).
 *
 * `null` for no project, a folder that has gone, or a workspace with no
 * `.aegis/roster/PROPOSAL.md`. A file that will not parse is a proposal with a
 * `problem`, not a rejection.
 */
export function rosterProposal(
  projectId: string | null,
): Promise<RosterProposal | null> {
  return call<RosterProposal | null>("roster_proposal", {
    project_id: projectId,
  });
}

/**
 * Creates the identities the open project's roster proposes: the grant.
 *
 * `digest` is the one {@link rosterProposal} returned. A file that changed
 * since is refused, so what is created is what was shown. Every new identity
 * or none; names already on file are skipped. There is no tool behind this —
 * a session can write a roster and cannot apply one.
 */
export function rosterApply(
  projectId: string,
  digest: string,
): Promise<RosterApplied> {
  return call<RosterApplied>("roster_apply", {
    project_id: projectId,
    digest,
  });
}

/** One identity's memories, most recently touched first. */
export function memoryList(agentId: string): Promise<Memory[]> {
  return call<Memory[]>("memory_list", { agent_id: agentId });
}

/**
 * Records a memory (`memoryId` null) or replaces one. A duplicate text touches
 * the existing memory and returns it. Refusals: `E_INVALID_SETTING` +
 * `error.field`.
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

/** Forgets one memory, returning it. There is no undo. */
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
 * Folds this session's older turns now (Phase 14) and returns the session,
 * folded or not. `E_TURN_BUSY` while a turn runs. Nothing is deleted; only what
 * reaches the model changes.
 */
export function sessionCompact(sessionId: string): Promise<SessionDetail> {
  return call<SessionDetail>("session_compact", { session_id: sessionId });
}

/**
 * Every routine (Phase 16), with a live `problem` when it cannot fire.
 */
export function routineList(): Promise<Routine[]> {
  return call<Routine[]>("routine_list");
}

/**
 * Creates (`routineId` null) or updates a routine, keeping its id and ledger.
 * The skill must be live, granted, and already run under watch; refusals are
 * `E_INVALID_SETTING` + `error.field`.
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
 * Pauses or resumes a routine. Resuming re-arms the clock (no catch-up run) and
 * clears the pause reason.
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
 * Fires a routine now, unattended exactly as the clock would. Resolves once
 * started; the rest arrives as `session:updated`, `routine:updated`, `turn:*`.
 */
export function routineRunNow(routineId: string): Promise<void> {
  return call<void>("routine_run_now", { routine_id: routineId });
}

// ---------------------------------------------------------------------------
// Connectors (PLAN 7.3, Phase 18)
// ---------------------------------------------------------------------------

/**
 * Every connector with its live state, tools, recent output and missing
 * environment variables.
 */
export function connectorList(): Promise<ConnectorView[]> {
  return call<ConnectorView[]>("connector_list");
}

/**
 * Creates (`connectorId` null) or replaces a connector and starts it. A failed
 * start is still saved; the row carries the server's output.
 */
export function connectorSave(
  connectorId: string | null,
  draft: ConnectorDraft,
): Promise<ConnectorView> {
  return call<ConnectorView>("connector_save", {
    connector_id: connectorId,
    draft,
  });
}

/**
 * Deletes a connector and stops it. Allow-lists naming its tools are left as
 * they are.
 */
export function connectorDelete(connectorId: string): Promise<void> {
  return call<void>("connector_delete", { connector_id: connectorId });
}

/** Starts or stops a connector without editing it. */
export function connectorSetEnabled(
  connectorId: string,
  enabled: boolean,
): Promise<ConnectorView> {
  return call<ConnectorView>("connector_set_enabled", {
    connector_id: connectorId,
    enabled,
  });
}

/** Restarts a failed or stopped connector. Nothing restarts on its own. */
export function connectorReconnect(
  connectorId: string,
): Promise<ConnectorView> {
  return call<ConnectorView>("connector_reconnect", {
    connector_id: connectorId,
  });
}
