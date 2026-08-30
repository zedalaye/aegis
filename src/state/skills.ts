/**
 * Skill catalog state (PLAN 7.3, Phase 13).
 *
 * The runbooks the library and the open project's `skills/` folder hold. Like
 * the shared-file panel, this store keeps no copy of anything: a `SKILL.md` is
 * a file in a folder somebody owns, and the answer to "what is there" is
 * measured on demand rather than remembered. A user who fixes a broken runbook
 * in their editor and presses refresh should see it become runnable.
 *
 * Three things this deliberately does not do.
 *
 * **It does not hold a body.** The catalog is a line per runbook — that is the
 * whole point of the phase, and a store that had fetched every body to render
 * a list would have rebuilt the thing the catalog exists to avoid. Opening one
 * is what a text editor is for; the path is on the row.
 *
 * **There is no action that writes one.** No editor, no template button. A
 * privileged write path from the window into the skill library would be a
 * second way to change what the agent will do, reachable without the approval
 * gate and absent from the audit log.
 *
 * **There is no action that runs one.** Running a skill is something a turn
 * does, on the model's initiative, under the identity's allow-list.
 */

import { create } from "zustand";

import type { Skill } from "../ipc/bindings";
import { skillList } from "../ipc/commands";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

/** Whether the catalog has been measured yet. */
export type LoadStatus = "idle" | "loading" | "ready" | "error";

export type SkillsState = {
  /** Every runbook found, workspace and library, by name. */
  readonly skills: readonly Skill[];
  readonly status: LoadStatus;
  readonly error: IpcError | null;

  /**
   * Measures the catalog for a project, or for the library alone.
   *
   * `null` is not "clear it": Settings is reachable with nothing open, and the
   * library is still worth listing there.
   */
  loadFor: (projectId: string | null) => Promise<void>;
  dismissError: () => void;
};

export const useSkills = create<SkillsState>((set) => ({
  skills: [],
  status: "idle",
  error: null,

  loadFor: async (projectId) => {
    set({ status: "loading" });
    try {
      set({ skills: await skillList(projectId), status: "ready", error: null });
    } catch (cause) {
      // Cleared rather than left stale: a list showing the previous project's
      // workspace runbooks under this project's name is worse than a panel
      // that says it could not look.
      set({
        skills: [],
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
