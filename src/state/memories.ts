/**
 * Per-identity memory state (PLAN 7.3, Phase 14).
 *
 * One identity's memories at a time. Writes from here are the human's own, so
 * they skip the gate. Refetched after each write, since a save may touch an
 * existing memory instead of adding one.
 */

import { create } from "zustand";

import type { Memory, MemoryDraft } from "../ipc/bindings";
import { memoryForget, memoryList, memorySave } from "../ipc/commands";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

/** Whether the memories have been fetched yet. */
export type LoadStatus = "idle" | "loading" | "ready" | "error";

export type MemoriesState = {
  /** Which identity the list below belongs to. */
  readonly agentId: string | null;
  /** That identity's memories, most recently touched first. */
  readonly memories: readonly Memory[];
  readonly status: LoadStatus;
  readonly error: IpcError | null;

  /** Fetches one identity's memories, replacing whatever was held. */
  loadFor: (agentId: string) => Promise<void>;
  /** Records a memory, or corrects the one named. */
  save: (
    agentId: string,
    memoryId: string | null,
    draft: MemoryDraft,
  ) => Promise<boolean>;
  /** Forgets one, and resolves with what went. */
  forget: (agentId: string, memoryId: string) => Promise<Memory | null>;
  dismissError: () => void;
};

export const useMemories = create<MemoriesState>((set, get) => ({
  agentId: null,
  memories: [],
  status: "idle",
  error: null,

  loadFor: async (agentId) => {
    // The identity is set before the fetch, so a panel that switched identity
    // mid-flight never draws the previous one's memories under the new name.
    set({ agentId, status: "loading" });
    try {
      const memories = await memoryList(agentId);
      // Dropped if the user has moved on: a slow response for an identity
      // nobody is looking at any more is stale by the time it lands.
      if (get().agentId === agentId) {
        set({ memories, status: "ready", error: null });
      }
    } catch (cause) {
      if (get().agentId === agentId) {
        set({
          memories: [],
          status: "error",
          error: toIpcError(cause, "memory_list"),
        });
      }
    }
  },

  save: async (agentId, memoryId, draft) => {
    try {
      await memorySave(agentId, memoryId, draft);
      await get().loadFor(agentId);
      return true;
    } catch (cause) {
      // Kept rather than cleared: this is a form error naming a field, and the
      // panel marks the input and leaves what was typed.
      set({ error: toIpcError(cause, "memory_save") });
      return false;
    }
  },

  forget: async (agentId, memoryId) => {
    try {
      const forgotten = await memoryForget(agentId, memoryId);
      await get().loadFor(agentId);
      return forgotten;
    } catch (cause) {
      set({ error: toIpcError(cause, "memory_forget") });
      return null;
    }
  },

  dismissError: () => set({ error: null }),
}));
