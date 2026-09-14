/**
 * Project state.
 *
 * A cache of the runtime's projects: mutations refetch rather than patch. The
 * display order is taken from the runtime once and then held (`stabilize`), so
 * opening a project does not move it under the cursor. Errors are held in
 * `error`, not thrown.
 */

import { create } from "zustand";

import type { Project, ProjectDetail } from "../ipc/bindings";
import {
  projectCreate,
  projectDelete,
  projectList,
  projectOpen,
  projectPickWorkspace,
  workspaceReveal,
} from "../ipc/commands";
import { folderName } from "../lib/format";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

/** Whether the list has been loaded yet. */
export type LoadStatus = "idle" | "loading" | "ready" | "error";

/** A picked workspace awaiting confirmation, so the name can be edited first. */
export type PendingWorkspace = {
  readonly path: string;
  readonly name: string;
};

/**
 * The result of a guarded command.
 *
 * A plain `T | undefined` cannot express "a void command succeeded", which is
 * most of them, so success is carried explicitly.
 */
type Outcome<T> = { readonly ok: true; readonly value: T } | { readonly ok: false };

export type ProjectsState = {
  /**
   * Every project, in a stable display order.
   *
   * Most recently opened first when the window starts; held in that order
   * afterwards, so clicking a project does not move it.
   */
  readonly projects: readonly Project[];
  /** The open project and its sessions, or `null` when none is open. */
  readonly detail: ProjectDetail | null;
  readonly status: LoadStatus;
  /** A workspace picked and awaiting confirmation. */
  readonly pending: PendingWorkspace | null;
  /** True while a command is in flight, so the UI can disable its buttons. */
  readonly busy: boolean;
  /** The last failure, or `null`. */
  readonly error: IpcError | null;

  /** Loads the list and opens the most recent project. */
  load: () => Promise<void>;
  /** Opens the native folder picker and stages the result. */
  pickWorkspace: () => Promise<void>;
  /** Edits the name of the staged workspace. */
  renamePending: (name: string) => void;
  /** Discards the staged workspace. */
  cancelPending: () => void;
  /** Creates the staged workspace as a project and opens it. */
  confirmPending: () => Promise<void>;
  /** Opens a project by id. */
  open: (projectId: string) => Promise<void>;
  /** Forgets a project. */
  remove: (projectId: string) => Promise<void>;
  /**
   * Opens this project's folder in the OS file manager.
   *
   * Failures land in `error` like every other command, but this one does not
   * take `busy`: revealing a folder is not a reason to disable the rail.
   */
  reveal: (projectId: string) => Promise<void>;
  /** Clears the last error. */
  dismissError: () => void;
};

export const useProjects = create<ProjectsState>((set, get) => {
  /**
   * The sidebar's order as ids: seeded from the runtime, then removed ids are
   * dropped and new ones go to the top; opened projects keep their place.
   */
  let order: readonly string[] = [];

  /** Reorders a freshly fetched list into the order the window is showing. */
  const stabilize = (projects: readonly Project[]): readonly Project[] => {
    const byId = new Map(projects.map((project) => [project.id, project]));
    const known = order
      .map((id) => byId.get(id))
      .filter((project): project is Project => project !== undefined);
    const knownIds = new Set(known.map((project) => project.id));
    const fresh = projects.filter((project) => !knownIds.has(project.id));

    const next = [...fresh, ...known];
    order = next.map((project) => project.id);
    return next;
  };

  /** Runs a command, holding any failure in `error` and always clearing `busy`. */
  const guard = async <T>(
    command: string,
    run: () => Promise<T>,
  ): Promise<Outcome<T>> => {
    set({ busy: true, error: null });
    try {
      return { ok: true, value: await run() };
    } catch (cause) {
      set({ error: toIpcError(cause, command) });
      return { ok: false };
    } finally {
      set({ busy: false });
    }
  };

  /**
   * Refetches and opens `preferredId`, else the most recently opened project.
   */
  const refresh = async (preferredId?: string): Promise<void> => {
    const projects = await projectList();
    // Before `stabilize`, because falling back to "the most recently opened"
    // is a question about recency and the runtime's order is the answer to it.
    // Only the *display* order is frozen.
    const target = projects.find((p) => p.id === preferredId) ?? projects.at(0);

    if (target === undefined) {
      set({ projects: stabilize(projects), detail: null, status: "ready" });
      return;
    }

    // Opening stamps `last_opened_at`, so the list is refetched afterwards
    // rather than before: what the runtime now holds is what a later start will
    // read. `stabilize` then puts it back into the order already on screen, so
    // the row the user just clicked does not move out from under them.
    const detail = await projectOpen(target.id);
    set({ projects: stabilize(await projectList()), detail, status: "ready" });
  };

  return {
    projects: [],
    detail: null,
    status: "idle",
    pending: null,
    busy: false,
    error: null,

    load: async () => {
      set({ status: "loading" });
      const outcome = await guard("project_list", () =>
        refresh(get().detail?.project.id),
      );
      if (!outcome.ok) {
        set({ status: "error" });
      }
    },

    pickWorkspace: async () => {
      const outcome = await guard("project_pick_workspace", projectPickWorkspace);
      // A cancelled dialog resolves to `null` and must leave the UI exactly as
      // it was; only a real choice stages anything.
      if (!outcome.ok || outcome.value === null) {
        return;
      }
      const path = outcome.value;
      set({ pending: { path, name: folderName(path) } });
    },

    renamePending: (name) => {
      const { pending } = get();
      if (pending !== null) {
        set({ pending: { ...pending, name } });
      }
    },

    cancelPending: () => set({ pending: null }),

    confirmPending: async () => {
      const { pending } = get();
      if (pending === null) {
        return;
      }

      const outcome = await guard("project_create", async () => {
        const project = await projectCreate(pending.name, pending.path);
        await refresh(project.id);
      });

      // The staged pick is cleared only once it is safely a project, so a
      // failed create leaves the form up with the path still in it.
      if (outcome.ok) {
        set({ pending: null });
      }
    },

    open: async (projectId) => {
      await guard("project_open", () => refresh(projectId));
    },

    remove: async (projectId) => {
      await guard("project_delete", async () => {
        await projectDelete(projectId);
        // No preferred id: whatever is most recent takes the open slot.
        await refresh();
      });
    },

    reveal: async (projectId) => {
      try {
        await workspaceReveal(projectId);
        set({ error: null });
      } catch (cause) {
        set({ error: toIpcError(cause, "workspace_reveal") });
      }
    },

    dismissError: () => set({ error: null }),
  };
});
