/**
 * Shared-workspace state (PLAN 7.3, Phase 11).
 *
 * The convention — `briefs/`, `status/`, `artefacts/`, `decisions/` — lives in
 * the user's own folder, so this store holds no copy of it. It holds the answer
 * to one question, "is it there right now", refetched rather than patched: a
 * user can create `decisions/` in a terminal between two renders, and a panel
 * that trusted its own cache would be confidently wrong about a directory it
 * does not own.
 *
 * Deliberately separate from the project store even though it follows the same
 * project. A project is a record Aegis keeps; this is a fact about someone
 * else's filesystem, measured on demand, and folding the two together would
 * mean every project refetch also went to disk.
 *
 * There is no action here for *writing* a decision or a status. That is
 * `fs_write` through the approval gate, driven by the conversation — a second
 * write path around the gate is exactly what this phase is not allowed to add.
 *
 * Errors are held rather than thrown, like every other store: the shell renders
 * the last one.
 */

import { create } from "zustand";

import type { WorkspaceLayout } from "../ipc/bindings";
import { workspaceLayout, workspaceScaffold } from "../ipc/commands";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

/** Whether the layout has been measured yet. */
export type LoadStatus = "idle" | "loading" | "ready" | "error";

export type WorkspaceState = {
  /** The convention's state in the open project, or `null` when none is. */
  readonly layout: WorkspaceLayout | null;
  readonly status: LoadStatus;
  /** True while scaffolding, so the button can say it is working. */
  readonly busy: boolean;
  /**
   * What the last scaffolding run created, for the line that reports it.
   *
   * Empty after a run that created nothing — which is the ordinary second run,
   * and worth saying out loud rather than leaving the user to guess whether
   * the button did anything.
   */
  readonly created: readonly string[];
  /** Whether a scaffolding run has finished since the panel was last loaded. */
  readonly scaffolded: boolean;
  readonly error: IpcError | null;

  /** Measures the convention in a project, or clears it for `null`. */
  loadFor: (projectId: string | null) => Promise<void>;
  /** Lays down what is missing, then re-measures. */
  scaffold: (projectId: string) => Promise<void>;
  dismissError: () => void;
};

export const useWorkspace = create<WorkspaceState>((set) => ({
  layout: null,
  status: "idle",
  busy: false,
  created: [],
  scaffolded: false,
  error: null,

  loadFor: async (projectId) => {
    if (projectId === null) {
      set({
        layout: null,
        status: "idle",
        created: [],
        scaffolded: false,
        error: null,
      });
      return;
    }

    set({ status: "loading", created: [], scaffolded: false });
    try {
      set({ layout: await workspaceLayout(projectId), status: "ready" });
    } catch (cause) {
      // The layout is cleared rather than left stale: a panel showing the
      // previous project's directories under this project's name is worse than
      // a panel that says it could not look.
      set({
        layout: null,
        status: "error",
        error: toIpcError(cause, "workspace_layout"),
      });
    }
  },

  scaffold: async (projectId) => {
    set({ busy: true, error: null });
    try {
      const report = await workspaceScaffold(projectId);
      // Re-measured from disk rather than derived from the report. The report
      // says what this run did; the panel shows what is there, and those come
      // apart the moment anything else touches the folder.
      set({
        layout: await workspaceLayout(projectId),
        status: "ready",
        created: report.created,
        scaffolded: true,
      });
    } catch (cause) {
      set({ error: toIpcError(cause, "workspace_scaffold") });
    } finally {
      set({ busy: false });
    }
  },

  dismissError: () => set({ error: null }),
}));
