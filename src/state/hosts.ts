/**
 * Execution-host state (PLAN 7.12).
 *
 * The machine's host options (fetched on mount and when the picker opens) and
 * setting a project's host, which refetches projects rather than patching.
 * Errors, notably `E_EXEC_HOST`, are held, not thrown.
 */

import { create } from "zustand";

import type { ExecHost, ExecHostOption } from "../ipc/bindings";
import { projectListExecHosts, projectSetExecHost } from "../ipc/commands";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

import { useProjects } from "./projects";

export type HostsState = {
  /**
   * Where a command could run, this computer first; empty until loaded (not
   * "only this computer").
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
