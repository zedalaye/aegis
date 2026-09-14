/**
 * Shared-workspace state (PLAN 7.3, Phase 11; PLAN 7.2 for the world).
 *
 * Measurements of the user's folder — the cabinet layout and the world —
 * always refetched, never patched. Nothing here creates a world or writes
 * files; that is `fs_write` under the gate. Separate from the project store so
 * a project refetch does not hit the disk. Errors are held, not thrown.
 */

import { create } from "zustand";

import type { WorkspaceLayout, WorldStatus } from "../ipc/bindings";
import {
  workspaceLayout,
  workspaceScaffold,
  worldStatus,
} from "../ipc/commands";
import { subscribe } from "../ipc/events";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

/** Whether the layout has been measured yet. */
export type LoadStatus = "idle" | "loading" | "ready" | "error";

export type WorkspaceState = {
  /** The project both measurements belong to, or `null` when none is open. */
  readonly projectId: string | null;
  /** The convention's state in the open project, or `null` when none is. */
  readonly layout: WorkspaceLayout | null;
  /**
   * The world in that folder, or `null` before measurement. Kept apart from
   * the layout: it costs more to measure (PLAN 7.2).
   */
  readonly world: WorldStatus | null;
  readonly status: LoadStatus;
  /** True while scaffolding, so the button can say it is working. */
  readonly busy: boolean;
  /** What the last scaffolding run created; empty is reported too. */
  readonly created: readonly string[];
  /**
   * Whether the last run created the git work tree (PLAN 7.11) — unlike
   * `layout.versioning`, which says whether it is versioned now.
   */
  readonly initialized: boolean;
  /** Why the folder is still not versioned (usually no `git`); not an error. */
  readonly problem: string | null;
  /** Whether a scaffolding run has finished since the panel was last loaded. */
  readonly scaffolded: boolean;
  readonly error: IpcError | null;

  /** Measures the convention in a project, or clears it for `null`. */
  loadFor: (projectId: string | null) => Promise<void>;
  /**
   * Re-measures the open project in place: unlike {@link loadFor}, no
   * `loading` state and the scaffold report is kept.
   */
  refresh: () => Promise<void>;
  /** Lays down what is missing, then re-measures. */
  scaffold: (projectId: string) => Promise<void>;
  dismissError: () => void;
};

export const useWorkspace = create<WorkspaceState>((set, get) => ({
  projectId: null,
  layout: null,
  world: null,
  status: "idle",
  busy: false,
  created: [],
  initialized: false,
  problem: null,
  scaffolded: false,
  error: null,

  loadFor: async (projectId) => {
    if (projectId === null) {
      set({
        projectId: null,
        layout: null,
        world: null,
        status: "idle",
        created: [],
        initialized: false,
        problem: null,
        scaffolded: false,
        error: null,
      });
      return;
    }

    set({
      projectId,
      status: "loading",
      created: [],
      initialized: false,
      problem: null,
      scaffolded: false,
    });
    try {
      const [layout, world] = await Promise.all([
        workspaceLayout(projectId),
        worldStatus(projectId),
      ]);
      set({ layout, world, status: "ready" });
    } catch (cause) {
      // The layout is cleared rather than left stale: a panel showing the
      // previous project's directories under this project's name is worse than
      // a panel that says it could not look.
      set({
        layout: null,
        world: null,
        status: "error",
        error: toIpcError(cause, "workspace_layout"),
      });
    }
  },

  refresh: async () => {
    const projectId = get().projectId;
    if (projectId === null) {
      return;
    }

    try {
      const [layout, world] = await Promise.all([
        workspaceLayout(projectId),
        worldStatus(projectId),
      ]);
      // Guarded, like the board's: a slow measurement that lands after somebody
      // opened another project must not draw that project's folder under this
      // one's name.
      if (get().projectId === projectId) {
        set({ layout, world, status: "ready" });
      }
    } catch (cause) {
      // Held, not surfaced. This runs on its own, after a turn nobody asked to
      // re-measure; a folder that has gone away is worth a line in the shell,
      // and it is not worth blanking two panels that were right a second ago.
      set({ error: toIpcError(cause, "workspace_layout") });
    }
  },

  scaffold: async (projectId) => {
    set({ busy: true, error: null });
    try {
      const report = await workspaceScaffold(projectId);
      // Re-measured from disk, world included, rather than derived from the
      // report.
      const [layout, world] = await Promise.all([
        workspaceLayout(projectId),
        worldStatus(projectId),
      ]);
      set({
        layout,
        world,
        status: "ready",
        created: report.created,
        initialized: report.initialized,
        problem: report.problem,
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

/**
 * Re-measures the folder when something may have changed it: on
 * `tool:approval_resolved` (every gated write, including all `world/` writes)
 * and `turn:finished` (the catch-all). Not `tool:finished`, which fires on
 * every read and would re-hash world sources.
 */
export function attachWorkspaceEvents(): Promise<() => void> {
  const again = () => {
    void useWorkspace.getState().refresh();
  };

  return subscribe({
    "tool:approval_resolved": again,
    "turn:finished": again,
  });
}
