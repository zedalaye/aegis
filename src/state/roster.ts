/**
 * Roster proposal state (PLAN 7.14).
 *
 * The open project's `.aegis/roster/PROPOSAL.md`, judged by the runtime against
 * the identities on file, and the one act this window can take on it: apply.
 *
 * Three decisions worth stating.
 *
 * **Measured, never remembered.** The proposal is a file a session or an editor
 * may rewrite at any moment, and which of its names already exist changes
 * whenever an identity is made. The panel re-reads it when it opens and after
 * every change to the identity list.
 *
 * **Apply sends back the digest it was shown.** The runtime refuses a file that
 * changed after the preview was built, because confirming is signing the
 * allow-lists on screen. This store never builds a draft of its own: what is
 * created is what the runtime read, not what the window holds.
 *
 * **Confirming is a second press.** The first opens a sentence saying exactly
 * what will be created; the second creates it. A grant of `fs_write` to three
 * identities is not a thing to do on one click that might have been meant for
 * Re-read.
 */

import { create } from "zustand";

import type { RosterApplied, RosterProposal } from "../ipc/bindings";
import { rosterApply, rosterProposal } from "../ipc/commands";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

import { useAgents } from "./agents";

/** Whether the proposal has been read yet. */
export type LoadStatus = "idle" | "loading" | "ready" | "error";

export type RosterState = {
  /** The proposal, or `null` when the open project has none. */
  readonly proposal: RosterProposal | null;
  /** The project it was read for. */
  readonly projectId: string | null;
  readonly status: LoadStatus;
  /** True while an apply is in flight. */
  readonly busy: boolean;
  /** Whether the confirmation is open. */
  readonly confirming: boolean;
  /** What the last apply did, until the proposal is read again for another project. */
  readonly applied: RosterApplied | null;
  /** The last refusal, said beside the proposal it is about. */
  readonly error: IpcError | null;

  /** Reads the proposal for a project; `null` clears it. */
  loadFor: (projectId: string | null) => Promise<void>;
  startConfirm: () => void;
  cancelConfirm: () => void;
  /** Applies what was shown, then re-reads the identities and the proposal. */
  apply: () => Promise<void>;
  dismissError: () => void;
};

export const useRoster = create<RosterState>((set, get) => ({
  proposal: null,
  projectId: null,
  status: "idle",
  busy: false,
  confirming: false,
  applied: null,
  error: null,

  loadFor: async (projectId) => {
    const sameProject = get().projectId === projectId;
    set({
      projectId,
      status: "loading",
      ...(sameProject ? {} : { applied: null, confirming: false, error: null }),
    });
    try {
      const proposal = await rosterProposal(projectId);
      // A slower read for a project that is no longer open must not land.
      if (get().projectId !== projectId) {
        return;
      }
      set({ proposal, status: "ready" });
    } catch (cause) {
      if (get().projectId !== projectId) {
        return;
      }
      set({
        proposal: null,
        status: "error",
        error: toIpcError(cause, "roster_proposal"),
      });
    }
  },

  startConfirm: () => set({ confirming: true, error: null }),
  cancelConfirm: () => set({ confirming: false }),

  apply: async () => {
    const { projectId, proposal } = get();
    if (projectId === null || proposal === null) {
      return;
    }

    set({ busy: true, error: null });
    try {
      const applied = await rosterApply(projectId, proposal.digest);
      set({ applied, confirming: false });
    } catch (cause) {
      set({ error: toIpcError(cause, "roster_apply"), confirming: false });
    } finally {
      set({ busy: false });
      // Either way the preview is stale: names now exist, or the file changed
      // under it. The panel re-reads the proposal when the identities land.
      await useAgents.getState().load();
      await get().loadFor(projectId);
    }
  },

  dismissError: () => set({ error: null }),
}));
