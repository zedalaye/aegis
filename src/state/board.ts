/**
 * Board state (PLAN 7.3, Phase 17; PLAN 7.22).
 *
 * A read of the runtime's board and traces: nothing is derived or written
 * here. Fetched only while the panel is open; events trigger a refetch, never
 * a patch. The one thing this panel does write is an answer to a parked ask,
 * which is the same three answers as the dialog's.
 */

import { create } from "zustand";
import type { UnlistenFn } from "@tauri-apps/api/event";

import type { Board, Decision, ParkedAsk, RunRef, RunTrace } from "../ipc/bindings";
import { boardRead, boardTrace, parkedAnswer, parkedList } from "../ipc/commands";
import { subscribe } from "../ipc/events";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

/** Whether the board has been read yet. */
export type LoadStatus = "idle" | "loading" | "ready" | "error";

/** Whether two references name the same run. */
export function sameRun(left: RunRef, right: RunRef): boolean {
  return (
    left.kind === right.kind &&
    left.id === right.id &&
    left.session_id === right.session_id
  );
}

export type BoardState = {
  /** Whether the panel is showing. */
  readonly open: boolean;
  /** The project the board on screen belongs to, or `null`. */
  readonly projectId: string | null;
  /** What the runtime last measured. */
  readonly board: Board | null;
  /** The calls its runs parked, oldest first (PLAN 7.22). */
  readonly parked: readonly ParkedAsk[];
  /** The parked asks an answer is in flight for. */
  readonly answering: readonly string[];
  readonly status: LoadStatus;
  /** The run whose lines are being read, and the lines. */
  readonly trace: RunTrace | null;
  /** True while a trace is being fetched. */
  readonly tracing: boolean;
  readonly error: IpcError | null;

  /** Shows the panel and reads the board of `projectId`. */
  openPanel: (projectId: string | null) => Promise<void>;
  closePanel: () => void;
  /** Re-reads, if the panel is open on a project. */
  refresh: () => Promise<void>;
  /** Follows the open project; a no-op while the panel is closed. */
  followProject: (projectId: string | null) => Promise<void>;
  /** Answers one parked ask, and picks its run up (PLAN 7.22). */
  answer: (parkedId: string, decision: Decision) => Promise<void>;
  /** Opens one run's replay, or closes it when it is already open. */
  toggleTrace: (run: RunRef) => Promise<void>;
  closeTrace: () => void;
  dismissError: () => void;
};

export const useBoard = create<BoardState>((set, get) => ({
  open: false,
  projectId: null,
  board: null,
  parked: [],
  answering: [],
  status: "idle",
  trace: null,
  tracing: false,
  error: null,

  openPanel: async (projectId) => {
    set({ open: true, projectId, trace: null });
    await get().refresh();
  },

  closePanel: () =>
    // The board is dropped rather than kept: reopening costs one read, and a
    // board held while the panel was shut is a board that is wrong by the time
    // anyone sees it again.
    set({ open: false, board: null, parked: [], status: "idle", trace: null }),

  refresh: async () => {
    const { open, projectId } = get();
    if (!open || projectId === null) {
      return;
    }

    set({ status: "loading" });
    try {
      // One await for both, so the columns and the cards under them are the
      // same measurement rather than two a second apart.
      const [board, parked] = await Promise.all([
        boardRead(projectId),
        parkedList(projectId),
      ]);
      // Guarded: a slow read that lands after the panel was closed, or after
      // the project changed, must not draw another project's board.
      if (get().open && get().projectId === projectId) {
        set({ board, parked, status: "ready", error: null });
      }
    } catch (cause) {
      set({ status: "error", error: toIpcError(cause, "board_read") });
    }
  },

  followProject: async (projectId) => {
    if (!get().open || get().projectId === projectId) {
      return;
    }
    set({ projectId, board: null, parked: [], trace: null });
    await get().refresh();
  },

  answer: async (parkedId, decision) => {
    if (get().answering.includes(parkedId)) {
      return;
    }
    set((state) => ({ answering: [...state.answering, parkedId], error: null }));

    try {
      await parkedAnswer(parkedId, decision);
    } catch (cause) {
      // The question is still open — a refused standing approval, a run that
      // is already going — so the board is re-read rather than guessed at.
      set({ error: toIpcError(cause, "parked_answer") });
    } finally {
      set((state) => ({
        answering: state.answering.filter((id) => id !== parkedId),
      }));
      await get().refresh();
    }
  },

  toggleTrace: async (run) => {
    const { projectId, trace } = get();
    if (projectId === null) {
      return;
    }
    if (trace !== null && sameRun(trace.run.run, run)) {
      set({ trace: null });
      return;
    }

    set({ tracing: true, error: null });
    try {
      set({ trace: await boardTrace(projectId, run) });
    } catch (cause) {
      // A run the panel is holding and the log has scrolled past. The board
      // itself is stale in that case, so it is re-read rather than left
      // showing a row that no longer answers.
      set({ error: toIpcError(cause, "board_trace"), trace: null });
      await get().refresh();
    } finally {
      set({ tracing: false });
    }
  },

  closeTrace: () => set({ trace: null }),

  dismissError: () => set({ error: null }),
}));

/**
 * Refetches the open board on events that change a column: `turn:started` and
 * `turn:finished` (In flight, run results), `tool:approval_*` and `parked:*`
 * (Attention), `routine:updated`. Not `audit:appended`, which fires per tool
 * call.
 */
export function attachBoardEvents(): Promise<UnlistenFn> {
  const again = () => {
    void useBoard.getState().refresh();
  };

  return subscribe({
    "turn:started": again,
    "turn:finished": again,
    "tool:approval_required": again,
    "tool:approval_resolved": again,
    "routine:updated": again,
    "parked:updated": again,
    "parked:resolved": again,
  });
}
