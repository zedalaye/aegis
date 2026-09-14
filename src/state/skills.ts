/**
 * Skill catalog state (PLAN 7.3, Phase 13).
 *
 * The skill catalog (library and open project), measured on demand, and the
 * project's proposals as a separate list so they cannot be granted (PLAN 7.13).
 * No bodies, and no actions that write or run a runbook.
 */

import { create } from "zustand";

import type { Skill, SkillProposal } from "../ipc/bindings";
import { skillList, skillProposals } from "../ipc/commands";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

/** Whether the catalog has been measured yet. */
export type LoadStatus = "idle" | "loading" | "ready" | "error";

export type SkillsState = {
  /** Every runbook found, workspace and library, by name. */
  readonly skills: readonly Skill[];
  /** Every `PROPOSAL.md` in the open project's workspace, by name. */
  readonly proposals: readonly SkillProposal[];
  readonly status: LoadStatus;
  readonly error: IpcError | null;

  /** Measures the catalog for a project, or the library alone for `null`. */
  loadFor: (projectId: string | null) => Promise<void>;
  dismissError: () => void;
};

export const useSkills = create<SkillsState>((set) => ({
  skills: [],
  proposals: [],
  status: "idle",
  error: null,

  loadFor: async (projectId) => {
    set({ status: "loading" });
    try {
      const [skills, proposals] = await Promise.all([
        skillList(projectId),
        skillProposals(projectId),
      ]);
      set({ skills, proposals, status: "ready", error: null });
    } catch (cause) {
      // Cleared rather than left stale: a list showing the previous project's
      // workspace runbooks under this project's name is worse than a panel
      // that says it could not look.
      set({
        skills: [],
        proposals: [],
        status: "error",
        error: toIpcError(cause, "skill_list"),
      });
    }
  },

  dismissError: () => set({ error: null }),
}));

/** The names an identity could be granted, in catalog order. */
export function skillNames(skills: readonly Skill[]): readonly string[] {
  return skills.map((skill) => skill.name);
}
