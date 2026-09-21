/**
 * Routine state (PLAN 7.3, Phase 16).
 *
 * Rows with a runtime-measured `problem`, updated by `routine:updated`. The
 * scheduler runs in Rust; there is no prompt field, a routine names a runbook.
 */

import { create } from "zustand";
import type { UnlistenFn } from "@tauri-apps/api/event";

import type { Routine, RoutineDraft } from "../ipc/bindings";
import { subscribe } from "../ipc/events";
import {
  routineDelete,
  routineList,
  routineRunNow,
  routineSave,
  routineSetPaused,
} from "../ipc/commands";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

import { useSpend } from "./spend";

/** Whether the list has been fetched yet. */
export type LoadStatus = "idle" | "loading" | "ready" | "error";

/** A blank form: hourly, a modest budget, signed for nothing. */
export function blankDraft(projectId: string, agentId: string): RoutineDraft {
  return {
    name: "",
    project_id: projectId,
    agent_id: agentId,
    skill: "",
    schedule: { kind: "every", minutes: 60 },
    // Empty on purpose. A routine that only reads is the safest thing this
    // form can produce, and every standing approval should be something
    // somebody decided to add.
    grants: [],
    runs_per_day: 24,
    spend: { per_run: null, per_day: null },
  };
}

/** The form filled in from an existing routine. */
export function draftOf(routine: Routine): RoutineDraft {
  return {
    name: routine.name,
    project_id: routine.project_id,
    agent_id: routine.agent_id,
    skill: routine.skill,
    schedule: routine.schedule,
    grants: [...routine.grants],
    runs_per_day: routine.runs_per_day,
    spend: { ...routine.spend },
  };
}

export type RoutinesState = {
  /** Every routine, by name. */
  readonly routines: readonly Routine[];
  readonly status: LoadStatus;
  /** True while a save, delete, pause or run is in flight. */
  readonly busy: boolean;
  /**
   * The routine the form is editing, `"new"` while creating one, or `null`
   * when the form is closed.
   */
  readonly editing: string | null;
  /** What the runtime refused, and which input it was about. */
  readonly fieldError: {
    readonly field: string;
    readonly message: string;
  } | null;
  /** A failure that is not about a form field. */
  readonly error: IpcError | null;

  load: () => Promise<void>;
  /** Replaces one row, from a `routine:updated` event. */
  observed: (routine: Routine) => void;
  startNew: () => void;
  startEdit: (routineId: string) => void;
  cancelEdit: () => void;
  /** Saves the form. Resolves `true` when the runtime accepted it. */
  save: (draft: RoutineDraft) => Promise<boolean>;
  remove: (routineId: string) => Promise<void>;
  setPaused: (routineId: string, paused: boolean) => Promise<void>;
  runNow: (routineId: string) => Promise<void>;
  dismissError: () => void;
};

/**
 * Splits a rejection into the half that belongs under an input and the half
 * that belongs in the shell's banner. The runtime decides which by setting
 * `field`, exactly as it does for identities and the provider.
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

export const useRoutines = create<RoutinesState>((set, get) => ({
  routines: [],
  status: "idle",
  busy: false,
  editing: null,
  fieldError: null,
  error: null,

  load: async () => {
    set({ status: "loading" });
    try {
      set({ routines: await routineList(), status: "ready", error: null });
    } catch (cause) {
      set({ status: "error", error: toIpcError(cause, "routine_list") });
    }
  },

  // Patched rather than refetched: this arrives whenever a run ends, which may
  // be at four in the morning with the panel open on something else, and a
  // refetch per event would be a list rebuilt for one changed row.
  observed: (routine) =>
    set((state) => ({
      routines: state.routines.some((held) => held.id === routine.id)
        ? state.routines.map((held) =>
            held.id === routine.id ? routine : held,
          )
        : [...state.routines, routine],
    })),

  startNew: () => set({ editing: "new", fieldError: null }),
  startEdit: (routineId) => set({ editing: routineId, fieldError: null }),
  cancelEdit: () => set({ editing: null, fieldError: null }),

  save: async (draft) => {
    const editing = get().editing;
    if (editing === null) {
      return false;
    }
    const creating = editing === "new";

    set({ busy: true, fieldError: null, error: null });
    try {
      await routineSave(creating ? null : editing, draft);
      // Refetched rather than patched: the runtime normalizes what it stores,
      // and the `problem` on every *other* row can change with this save — a
      // second routine on the same identity may now be over its ceiling.
      set({ routines: await routineList(), status: "ready", editing: null });
      return true;
    } catch (cause) {
      set(landing(cause, "routine_save"));
      return false;
    } finally {
      set({ busy: false });
    }
  },

  remove: async (routineId) => {
    set({ busy: true, error: null });
    try {
      await routineDelete(routineId);
      set({ routines: await routineList(), status: "ready" });
      if (get().editing === routineId) {
        set({ editing: null, fieldError: null });
      }
    } catch (cause) {
      set({ error: toIpcError(cause, "routine_delete") });
    } finally {
      set({ busy: false });
    }
  },

  setPaused: async (routineId, paused) => {
    set({ busy: true, error: null });
    try {
      get().observed(await routineSetPaused(routineId, paused));
    } catch (cause) {
      set({ error: toIpcError(cause, "routine_set_paused") });
    } finally {
      set({ busy: false });
    }
  },

  runNow: async (routineId) => {
    set({ busy: true, error: null });
    try {
      // Resolves when the run has *started*. What it did arrives as a
      // `routine:updated` when it ends, like any other run — there is nothing
      // here to wait on, and a spinner that waited would be pretending a
      // fifteen-minute run is a button press.
      await routineRunNow(routineId);
    } catch (cause) {
      set({ error: toIpcError(cause, "routine_run_now") });
    } finally {
      set({ busy: false });
    }
  },

  dismissError: () => set({ error: null, fieldError: null }),
}));

/** Patches rows from `routine:updated`, one message per run. */
export function attachRoutineEvents(): Promise<UnlistenFn> {
  return subscribe({
    "routine:updated": (routine) => {
      useRoutines.getState().observed(routine);
      // A run that ended has spent something (PLAN 7.26).
      void useSpend.getState().refresh();
    },
  });
}
