/**
 * Today's model spend, by routine and identity (PLAN 7.26).
 *
 * Read on demand — when a list or a form that shows it mounts, and when a run
 * ends — never polled. The figure is the ledger's; nothing here can write it.
 */

import { create } from "zustand";

import type { SpendToday } from "../ipc/bindings";
import { spendToday } from "../ipc/commands";

export type SpendState = {
  /** Micro-dollars by id; empty until loaded, which reads as nothing spent. */
  readonly today: SpendToday;
  /** Asks the runtime again. A failure keeps the last figures. */
  refresh: () => Promise<void>;
};

export const useSpend = create<SpendState>((set) => ({
  today: { routines: {}, agents: {} },

  refresh: async () => {
    try {
      set({ today: await spendToday() });
    } catch {
      // A figure that could not be read is not shown as zero: the last one
      // stays, and the caps are enforced in Rust whatever this says.
    }
  },
}));
