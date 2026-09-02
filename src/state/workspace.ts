/**
 * Shared-workspace state (PLAN 7.3, Phase 11; PLAN 7.2 for the world).
 *
 * The convention — `briefs/`, `status/`, `artefacts/`, `decisions/` — lives in
 * the user's own folder, so this store holds no copy of it. It holds the answer
 * to one question, "is it there right now", refetched rather than patched: a
 * user can create `decisions/` in a terminal between two renders, and a panel
 * that trusted its own cache would be confidently wrong about a directory it
 * does not own.
 *
 * The world is the second layer in that same folder, measured beside the first
 * and on the same terms. There is no action for *creating* it, and that is the
 * point of it being opt-in: five empty templates in a workspace with no essence
 * are theatre, so a world starts when somebody writes `world/essence.md` and
 * this store reports what they wrote.
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
   * The world in that same folder, or `null` when none has been measured.
   *
   * Beside the layout rather than folded into it: they are two layers with
   * opposite mutation rules (PLAN 7.2), and the measurement costs differently —
   * the cabinet is a handful of `stat` calls, the world reads the sources it
   * declares. `present: false` is the ordinary answer and not an error.
   */
  readonly world: WorldStatus | null;
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
  /**
   * Re-measures the open project, in place.
   *
   * Not {@link loadFor}: this runs while somebody is looking at the panel, so
   * it must not pass through `loading` — which blanks both panels — and must not
   * clear the line reporting what the last scaffold created. It changes what is
   * drawn only when the folder has actually changed.
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
        scaffolded: false,
        error: null,
      });
      return;
    }

    set({ projectId, status: "loading", created: [], scaffolded: false });
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
      // Re-measured from disk rather than derived from the report. The report
      // says what this run did; the panel shows what is there, and those come
      // apart the moment anything else touches the folder.
      //
      // The world is re-measured with it even though scaffolding never touches
      // `world/`: the button is the moment somebody looks at this panel, and a
      // constitution written in an editor since the project was opened is
      // exactly what would otherwise be stale.
      const [layout, world] = await Promise.all([
        workspaceLayout(projectId),
        worldStatus(projectId),
      ]);
      set({
        layout,
        world,
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

/**
 * Re-measures the folder when something may have changed it.
 *
 * Both panels are a picture of somebody else's directory, and until this
 * existed they were measured once — when the project opened — and never again.
 * A session that wrote `world/essence.md` through the gate left the World panel
 * saying *not written* about a file it had just created, which reads as a
 * broken panel rather than as a stale one.
 *
 * Two events, and neither is `tool:finished`: that fires for every read as well,
 * and re-measuring is the expensive half here — `world_status` hashes every
 * declared source. What is used instead is the pair the board already uses, for
 * the same reason it does.
 *
 * * **`tool:approval_resolved`** — every gated write passes through it, and a
 *   write into `world/` is *always* gated, so the panel follows a session
 *   founding a world file by file.
 * * **`turn:finished`** — the catch-all, once per turn. It covers what the
 *   first misses: a write running under a standing grant, a `shell_exec` that
 *   touched the folder, a connector's own tool.
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
