/**
 * Typed `invoke` wrappers — one function per Rust command.
 *
 * Components never call `invoke` directly: the command name and the argument
 * shape are spelled out exactly once, here, and every rejection is normalized
 * to an {@link IpcError}. Payload *types* move the other way — from Phase 5
 * they are generated into `./bindings.ts` by `ts-rs`, so this file imports
 * them rather than restating them.
 *
 * Commands land with their phases: the window and application lifecycle
 * (PLAN 2.1, "Window / tray"), projects (PLAN 2.1, "Projects") and the audit
 * log (PLAN 2.1, "Settings and audit").
 *
 * Argument keys are `snake_case`, matching the Rust parameter names — the
 * commands are declared `rename_all = "snake_case"`, so the camelCase Tauri
 * would otherwise accept is deliberately not part of the contract.
 */

import { invoke } from "@tauri-apps/api/core";
import type { InvokeArgs } from "@tauri-apps/api/core";

import { toIpcError } from "../lib/errors";
import type { AuditEntry, Project, ProjectDetail } from "./bindings";

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
