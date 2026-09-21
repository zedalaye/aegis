/**
 * Identity state (PLAN 7.3, Phase 12).
 *
 * Identities plus the edit form's state. Mutations refetch, since the runtime
 * normalizes what it stores. Field refusals stay beside the form; other
 * failures go to {@link AgentsState.error}.
 */

import { create } from "zustand";

import type { Agent, AgentDraft } from "../ipc/bindings";
import {
  agentCreate,
  agentDelete,
  agentList,
  agentUpdate,
} from "../ipc/commands";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";
import { DEFAULT_PROVIDER_ID } from "./settings";

/** Whether the list has been fetched yet. */
export type LoadStatus = "idle" | "loading" | "ready" | "error";

export { DEFAULT_PROVIDER_ID } from "./settings";

/** The identity a session gets when none is chosen. */
export const DEFAULT_AGENT_ID = "default";

/** An empty form, for creating an identity. */
export function blankDraft(): AgentDraft {
  return {
    name: "",
    role: "",
    instructions: "",
    provider_id: DEFAULT_PROVIDER_ID,
    model: "",
    tools: [],
    skills: [],
    runs_per_day: DEFAULT_RUNS_PER_DAY,
    spend: { per_run: null, per_day: null },
  };
}

/**
 * A copy of an identity's perimeter (role, instructions, allow-lists, budget)
 * without its memories or run history (`COS.md`).
 */
export function cloneDraft(agent: Agent): AgentDraft {
  return {
    ...draftOf(agent),
    name: `${agent.name} copy`,
  };
}

/**
 * The scheduled runs a new identity is allowed in a day.
 *
 * Mirrors `AGENT_RUNS_PER_DAY_DEFAULT` in `store/agents.rs`. It bounds only
 * what a clock may start as this identity; a person typing is not budgeted.
 */
const DEFAULT_RUNS_PER_DAY = 48;

/** The form filled in from an existing identity. */
export function draftOf(agent: Agent): AgentDraft {
  return {
    name: agent.name,
    role: agent.role,
    instructions: agent.instructions,
    provider_id: agent.provider_id,
    model: agent.model,
    tools: [...agent.tools],
    skills: [...agent.skills],
    runs_per_day: agent.runs_per_day,
    spend: { ...agent.spend },
  };
}

export type AgentsState = {
  /** Every identity, built-in first. */
  readonly agents: readonly Agent[];
  readonly status: LoadStatus;
  /** True while a create, update or delete is in flight. */
  readonly busy: boolean;
  /**
   * The identity the form is editing, `"new"` while creating one, or `null`
   * when the form is closed.
   */
  readonly editing: string | null;
  /**
   * What the form should open filled in with, when it is not opening on an
   * existing identity: a clone's fields, or `null` for a blank one.
   */
  readonly seed: AgentDraft | null;
  /** What the runtime refused, and which input it was about. */
  readonly fieldError: { readonly field: string; readonly message: string } | null;
  /** A failure that is not about a form field. */
  readonly error: IpcError | null;

  /** Fetches the list. */
  load: () => Promise<void>;
  /** Opens the form on a new identity. */
  startNew: () => void;
  /** Opens the form on a new identity shaped like an existing one. */
  startClone: (agent: Agent) => void;
  /** Opens the form on an existing one. */
  startEdit: (agentId: string) => void;
  /** Closes the form, discarding whatever was typed. */
  cancelEdit: () => void;
  /**
   * Saves the form — creating or replacing, depending on what it was opened
   * on. Resolves to `true` when the runtime accepted it, so the caller knows
   * whether the form closed.
   */
  save: (draft: AgentDraft) => Promise<boolean>;
  /** Deletes an identity. */
  remove: (agentId: string) => Promise<void>;
  dismissError: () => void;
};

/** Routes a rejection under its input when the runtime set `field`. */
function landing(cause: unknown, command: string) {
  const error = toIpcError(cause, command);
  return error.field === null
    ? { error, fieldError: null }
    : {
        error: null,
        fieldError: { field: error.field, message: error.message },
      };
}

export const useAgents = create<AgentsState>((set, get) => ({
  agents: [],
  status: "idle",
  busy: false,
  editing: null,
  seed: null,
  fieldError: null,
  error: null,

  load: async () => {
    set({ status: "loading" });
    try {
      set({ agents: await agentList(), status: "ready" });
    } catch (cause) {
      set({ status: "error", error: toIpcError(cause, "agent_list") });
    }
  },

  startNew: () => set({ editing: "new", seed: null, fieldError: null }),
  startClone: (agent) =>
    set({ editing: "new", seed: cloneDraft(agent), fieldError: null }),
  startEdit: (agentId) =>
    set({ editing: agentId, seed: null, fieldError: null }),
  cancelEdit: () => set({ editing: null, seed: null, fieldError: null }),

  save: async (draft) => {
    const editing = get().editing;
    if (editing === null) {
      return false;
    }
    const creating = editing === "new";

    set({ busy: true, fieldError: null, error: null });
    try {
      if (creating) {
        await agentCreate(draft);
      } else {
        await agentUpdate(editing, draft);
      }
      // Refetched rather than patched: the runtime normalizes what it stores.
      set({
        agents: await agentList(),
        status: "ready",
        editing: null,
        seed: null,
      });
      return true;
    } catch (cause) {
      set(landing(cause, creating ? "agent_create" : "agent_update"));
      return false;
    } finally {
      set({ busy: false });
    }
  },

  remove: async (agentId) => {
    set({ busy: true, error: null });
    try {
      await agentDelete(agentId);
      set({ agents: await agentList(), status: "ready" });
      // The form may have been open on what just went.
      if (get().editing === agentId) {
        set({ editing: null, seed: null, fieldError: null });
      }
    } catch (cause) {
      set({ error: toIpcError(cause, "agent_delete") });
    } finally {
      set({ busy: false });
    }
  },

  dismissError: () => set({ error: null, fieldError: null }),
}));
