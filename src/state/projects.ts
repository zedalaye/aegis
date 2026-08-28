/**
 * Project state.
 *
 * The runtime owns the truth; this store is a cache of it plus the transient
 * UI state around the "add a workspace" flow. Every mutation therefore ends by
 * refetching the list rather than by patching it locally — the runtime already
 * reorders projects by recency and deduplicates workspaces, and guessing at
 * those rules here is how the two drift apart.
 *
 * Errors are held rather than thrown. Every action resolves; a failure lands
 * in `error` for the shell to render, because there is no boundary above these
 * calls that could do anything better with a rejection.
 */

import { create } from "zustand";

import type { Project, ProjectDetail } from "../ipc/bindings";
import {
  projectCreate,
  projectDelete,
  projectList,
  projectOpen,
  projectPickWorkspace,
} from "../ipc/commands";
import { folderName } from "../lib/format";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

/** Whether the list has been loaded yet. */
export type LoadStatus = "idle" | "loading" | "ready" | "error";

/**
 * A workspace the user has picked but not yet confirmed.
 *
 * Naming is a separate step from picking so the proposed name is editable
 * before anything is written — `project_create` takes a name, and asking for
 * it after the fact would need a rename command the MVP does not have.
 */
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
  /** Every project, most recently opened first. */
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
  /** Clears the last error. */
  dismissError: () => void;
};

export const useProjects = create<ProjectsState>((set, get) => {
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
   * Refetches the list and leaves exactly one project open.
   *
   * `preferredId` is the project the caller just acted on. Falling back to the
   * first entry — the most recently opened — is what makes a restart land
   * where the user left off, and what fills the empty pane after a delete.
   */
  const refresh = async (preferredId?: string): Promise<void> => {
    const projects = await projectList();
    const target = projects.find((p) => p.id === preferredId) ?? projects.at(0);

    if (target === undefined) {
      set({ projects, detail: null, status: "ready" });
      return;
    }

    // Opening stamps `last_opened_at`, so the list is refetched afterwards
    // rather than before: the order the sidebar shows must be the order the
    // runtime now holds.
    const detail = await projectOpen(target.id);
    set({ projects: await projectList(), detail, status: "ready" });
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

    dismissError: () => set({ error: null }),
  };
});
