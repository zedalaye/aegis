/**
 * Identity state (PLAN 7.3, Phase 12).
 *
 * The identities a session can be opened as, plus the one thing the panel needs
 * that the runtime does not hold: which identity is being edited, and which
 * field of it was refused.
 *
 * Two decisions worth stating.
 *
 * **The list is refetched after every mutation rather than patched.** The
 * runtime normalizes what it stores — a tool list comes back in registry order,
 * a name comes back trimmed — so a locally patched row would differ from the
 * one on disk in exactly the ways that are hard to notice. It is a handful of
 * records changed by a human a few times a month; there is nothing to optimize.
 *
 * **A refused field is held here, beside the form, not in the shell's banner.**
 * The runtime says which input it was talking about, and a message under that
 * input is worth more than the same message across the top of the window. Only
 * failures that are *not* about a field — a delete refused because sessions
 * still run as the identity — go to {@link AgentsState.error}, which the shell
 * renders like every other store's.
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

/** Whether the list has been fetched yet. */
export type LoadStatus = "idle" | "loading" | "ready" | "error";

/**
 * The provider binding this build can resolve.
 *
 * Mirrors `DEFAULT_PROVIDER_ID` in `store/agents.rs`. The form does not offer a
 * choice, because there is not one to make yet — a roster of providers to bind
 * identities to is later work (PLAN 7.1, *Provider*) — but the value still has
 * to be sent, because the runtime validates it rather than filling it in.
 */
export const DEFAULT_PROVIDER_ID = "default";

/** The identity a session gets when none is chosen. */
export const DEFAULT_AGENT_ID = "default";

/** An empty form, for creating an identity. */
export function blankDraft(): AgentDraft {
  return {
    name: "",
    role: "",
    instructions: "",
    provider_id: DEFAULT_PROVIDER_ID,
    tools: [],
    skills: [],
    runs_per_day: DEFAULT_RUNS_PER_DAY,
  };
}

/**
 * A new identity shaped like an existing one (PLAN 7.3, Phase 16).
 *
 * "Clone a role without cloning its rotten memory" (`COS.md`). What is copied
 * is the perimeter — the role, the instructions, the two allow-lists, the
 * budget. What is not copied is everything the copy would have to *earn*: its
 * memories, which belong to an identity and are keyed on its id, and the fact
 * that somebody has watched it run a runbook. A clone starts with a clean
 * record on purpose; that is the whole point of making one.
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
    tools: [...agent.tools],
    skills: [...agent.skills],
    runs_per_day: agent.runs_per_day,
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

/**
 * Splits a rejection into the half that belongs under an input and the half
 * that belongs in the shell's banner.
 *
 * The runtime is the one that decides: it sets `field` when the failure is
 * about a value the user typed, and leaves it off otherwise.
 */
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
