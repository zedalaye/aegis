/**
 * Board state (PLAN 7.3, Phase 17).
 *
 * The panel that answers "who ran, what did it cost, why did it fail" without
 * opening a chat. Four decisions shape this store.
 *
 * **It holds no truth of its own.** Every line on the board is measured by the
 * runtime on the read: the file half comes off disk, the live half from the
 * turn registry, the approval registry and the routine document, and the runs
 * are folded out of `audit.jsonl`. Nothing here derives a status, and there is
 * no command anywhere in the WebView that writes one — a board the window could
 * edit would stop being a record and become a second opinion.
 *
 * **It reads only while the panel is open.** A board is a composition over four
 * stores and a log; paying for it when nobody is looking would be a file read
 * per turn for a panel that is not on screen. Open fetches, closed forgets.
 *
 * **It refetches rather than patches.** The audit drawer can prepend the one
 * line the runtime just wrote, because a line is self-contained. A board is
 * not: one finished turn can move a run between two columns, change a total and
 * add an artefact. So the events it listens to are triggers to ask again rather
 * than deltas to apply, and they are the ones that mark a *column* changing —
 * never `audit:appended`, which fires per tool call.
 *
 * **A trace is fetched, never assembled here.** The detail pane shows the audit
 * lines of one run in the order they happened; they come from the runtime as
 * they are on disk. A window that rendered a story it had assembled itself
 * would be worth less than the file.
 */

import { create } from "zustand";
import type { UnlistenFn } from "@tauri-apps/api/event";

import type { Board, RunRef, RunTrace } from "../ipc/bindings";
import { boardRead, boardTrace } from "../ipc/commands";
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
  /** Opens one run's replay, or closes it when it is already open. */
  toggleTrace: (run: RunRef) => Promise<void>;
  closeTrace: () => void;
  dismissError: () => void;
};

export const useBoard = create<BoardState>((set, get) => ({
  open: false,
  projectId: null,
  board: null,
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
    set({ open: false, board: null, status: "idle", trace: null }),

  refresh: async () => {
    const { open, projectId } = get();
    if (!open || projectId === null) {
      return;
    }

    set({ status: "loading" });
    try {
      const board = await boardRead(projectId);
      // Guarded: a slow read that lands after the panel was closed, or after
      // the project changed, must not draw another project's board.
      if (get().open && get().projectId === projectId) {
        set({ board, status: "ready", error: null });
      }
    } catch (cause) {
      set({ status: "error", error: toIpcError(cause, "board_read") });
    }
  },

  followProject: async (projectId) => {
    if (!get().open || get().projectId === projectId) {
      return;
    }
    set({ projectId, board: null, trace: null });
    await get().refresh();
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
 * Keeps the board in step with what the runtime is doing.
 *
 * One event per column, which is the rule for what belongs here: a board is a
 * composition rather than a list of lines, so the question is never "did
 * something happen" but "did one of these three columns change".
 *
 * * `turn:started` and `turn:finished` are **In flight** opening and closing —
 *   and the second is also when a run's status, its cost and its artefacts all
 *   settle at once.
 * * `tool:approval_required` and `tool:approval_resolved` are **Attention**.
 *   Without them the most urgent column would be the least current, which is
 *   the one thing a board must not be: a question raised while somebody is
 *   looking at the board would sit there until they pressed Refresh.
 * * `routine:updated` is a clock that stopped itself, or a run that ended while
 *   nobody was looking.
 *
 * `audit:appended` is deliberately absent. It fires per tool call, and a board
 * re-read six times inside one turn would be six compositions over four stores
 * and a log to show the same three columns.
 *
 * All of them are no-ops while the panel is shut: `refresh` returns immediately
 * unless the board is open on a project.
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
  });
}
