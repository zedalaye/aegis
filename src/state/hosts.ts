/**
 * Execution-host state (PLAN 7.12).
 *
 * Two facts, with different lifetimes, which is why this is its own store and
 * not a corner of {@link useProjects}.
 *
 * The *options* are a fact about the machine — which WSL distributions are
 * installed — and they change when somebody installs one in a terminal, not
 * when a project is opened. They are asked for once on mount and re-asked only
 * when a person opens the picker, because a list that is confidently out of
 * date is worse than one that takes a moment.
 *
 * The *choice* is a fact about the project, and it lives on the project record
 * where it belongs. Setting it here therefore ends by refetching the projects:
 * the runtime is the only thing that knows whether the host it was handed can
 * actually be used, and a store that patched its own copy optimistically would
 * be drawing a distribution the runtime had just refused.
 *
 * Errors are held rather than thrown, like every other store. `E_EXEC_HOST` is
 * the one worth reading — it is the runtime saying which distributions this
 * machine really has, or that this folder is not one the chosen distribution
 * can reach.
 */

import { create } from "zustand";

import type { ExecHost, ExecHostOption } from "../ipc/bindings";
import { projectListExecHosts, projectSetExecHost } from "../ipc/commands";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

import { useProjects } from "./projects";

export type HostsState = {
  /**
   * Everywhere a command could run, this computer first.
   *
   * Empty until the first load, which is not the same as "only this computer":
   * a panel that drew a picker before it had asked would offer one row and then
   * grow, which reads as the list having changed.
   */
  readonly options: readonly ExecHostOption[];
  /** Whether the options have been asked for yet. */
  readonly loaded: boolean;
  /** True while a choice is being saved, so the picker can disable itself. */
  readonly busy: boolean;
  readonly error: IpcError | null;

  /** Asks the runtime what this machine can offer. */
  load: () => Promise<void>;
  /** Says where a project's commands run; `null` is this computer. */
  set: (projectId: string, host: ExecHost | null) => Promise<void>;
  dismissError: () => void;
};

export const useHosts = create<HostsState>((set) => ({
  options: [],
  loaded: false,
  busy: false,
  error: null,

  load: async () => {
    try {
      set({ options: await projectListExecHosts(), loaded: true });
    } catch (cause) {
      // `loaded` stays false: the picker draws nothing rather than claiming
      // this machine has no distributions when the truth is that nobody asked
      // successfully.
      set({ error: toIpcError(cause, "project_list_exec_hosts") });
    }
  },

  set: async (projectId, host) => {
    set({ busy: true, error: null });
    try {
      await projectSetExecHost(projectId, host);
      // Through the project store rather than by patching a local copy: the
      // host is on the project record, the rail and the title bar read it from
      // there, and one source of truth is what keeps them from disagreeing.
      await useProjects.getState().open(projectId);
    } catch (cause) {
      set({ error: toIpcError(cause, "project_set_exec_host") });
    } finally {
      set({ busy: false });
    }
  },

  dismissError: () => set({ error: null }),
}));
