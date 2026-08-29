/**
 * Session and transcript state.
 *
 * The runtime owns the truth. This store holds a cache of it plus the one
 * thing that has no counterpart on disk: the buffer a reply streams into
 * before it is finalized.
 *
 * The reconciliation rule is worth stating, because everything else follows
 * from it. **Events render, `turn:finished` reconciles.** Deltas and finalized
 * messages are applied locally so the transcript moves while the turn runs;
 * when the turn ends the session is re-opened and the authoritative transcript
 * replaces whatever was assembled here. That keeps the optimistic path simple
 * — it never has to be exactly right, only close — while guaranteeing that
 * what is on screen after a turn is what is on disk. It is also what fills in
 * the parts the UI never sees streamed, such as the `tool` messages carrying
 * result envelopes.
 *
 * Two filters are applied to incoming events:
 *
 * - Anything for a session other than the open one is ignored, except
 *   `session:updated`, which keeps the sidebar honest for every row.
 * - `turn:delta` and `tool:progress` are dropped unless their `seq` is greater
 *   than the last one applied, so a duplicated or reordered frame cannot
 *   double a word.
 *
 * Live command output is the one thing here that reconciliation does *not*
 * replace. It has no counterpart on disk — the runtime keeps a one-line
 * summary and an audit line, not the transcript of every build — so the pane
 * is kept in this store, keyed by call, for as long as the session stays open.
 */

import { create } from "zustand";

import type {
  Message,
  SessionDetail,
  SessionSummary,
  Stream,
} from "../ipc/bindings";
import {
  sessionCancel,
  sessionCreate,
  sessionDelete,
  sessionList,
  sessionOpen,
  sessionRename,
  sessionSend,
} from "../ipc/commands";
import { subscribe } from "../ipc/events";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

/** A reply being streamed, between `turn:started` and `turn:finished`. */
export type Streaming = {
  /** The turn, for cancelling it. */
  readonly turnId: string;
  /**
   * The model the runtime started this turn with.
   *
   * Reported rather than assumed: the provider is chosen per turn from
   * settings, so this is the only thing that knows what is actually writing
   * the text arriving on screen. `aegis-fake-1` when that is the truth.
   */
  readonly model: string;
  /** Text accumulated from `turn:delta` since the last finalized message. */
  readonly text: string;
  /** Highest `seq` applied. Frames at or below it are duplicates. */
  readonly seq: number;
};

/** One unbroken run of output from the same pipe. */
export type OutputRun = {
  /** Which pipe it came from. */
  readonly stream: Stream;
  /** The text. */
  readonly text: string;
};

/** What a running — or finished — command printed. */
export type ToolOutput = {
  /**
   * The runs, in arrival order. Consecutive frames from the same pipe are
   * merged, so a command printing a megabyte does not become a million nodes.
   */
  readonly runs: readonly OutputRun[];
  /** Highest `seq` applied. Frames at or below it are duplicates. */
  readonly seq: number;
  /** Whether the command produced more than what is shown. */
  readonly truncated: boolean;
};

/** How much text one call's pane keeps before it starts dropping the front. */
const OUTPUT_MAX_CHARS = 64 * 1024;

export type SessionsState = {
  /** The open project's sessions, most recently active first. */
  readonly sessions: readonly SessionSummary[];
  /** The open session, or `null`. */
  readonly detail: SessionDetail | null;
  /** The reply in flight, or `null` when nothing is streaming. */
  readonly streaming: Streaming | null;
  /** What each tool call has printed, by `call_id`. */
  readonly output: Readonly<Record<string, ToolOutput>>;
  /** True while a command is in flight. */
  readonly busy: boolean;
  /** The last failure, or `null`. */
  readonly error: IpcError | null;

  /** Loads a project's sessions and opens the most recent one. */
  loadFor: (projectId: string) => Promise<void>;
  /** Opens a session by id. */
  open: (sessionId: string) => Promise<void>;
  /** Creates a session in a project and opens it. */
  create: (projectId: string) => Promise<void>;
  /** Renames a session. */
  rename: (sessionId: string, title: string) => Promise<void>;
  /** Deletes a session. */
  remove: (sessionId: string) => Promise<void>;
  /** Sends a message in the open session. */
  send: (text: string) => Promise<void>;
  /** Cancels the running turn, if there is one. */
  cancel: () => Promise<void>;
  /** Clears the last error. */
  dismissError: () => void;
  /** Forgets everything. Called when the open project changes. */
  reset: () => void;
};

/** Whether a message should be drawn in the transcript. */
export function isVisible(message: Message): boolean {
  // `tool` messages carry a result envelope written for the model — raw JSON,
  // sometimes kilobytes of it. What a person needs from a tool call is the
  // one-line summary on the call record, which the assistant message holds.
  // `system` never reaches the transcript at all; it is rebuilt per request.
  return message.role === "user" || message.role === "assistant";
}

/**
 * Sorts sessions the way the runtime does: most recently active first.
 *
 * Applied again here because a `session:updated` bumps `updated_at`, and a
 * sidebar that only re-sorted on a refetch would leave the session the user is
 * typing in halfway down the list. Timestamps are fixed-width UTC RFC3339, so
 * comparing them as strings is comparing them as instants.
 */
/**
 * Adds a frame to what a call has printed.
 *
 * Consecutive frames from the same pipe are merged into one run: a command
 * printing steadily produces a frame every 50 ms, and one node per frame would
 * be thousands of nodes for a build that says nothing interesting. The pane
 * keeps the *end* of a long run rather than the start — the tail of a build log
 * is the part someone watching it is waiting for.
 */
function appended(
  current: ToolOutput | undefined,
  stream: Stream,
  text: string,
  seq: number,
): ToolOutput {
  const previous = current?.runs ?? [];
  const last = previous.at(-1);
  const runs =
    last !== undefined && last.stream === stream
      ? [...previous.slice(0, -1), { stream, text: last.text + text }]
      : [...previous, { stream, text }];

  return {
    runs: capped(runs),
    seq,
    truncated: current?.truncated ?? false,
  };
}

/** Drops the front of a pane that has grown past what it will hold. */
function capped(runs: readonly OutputRun[]): OutputRun[] {
  let total = runs.reduce((sum, run) => sum + run.text.length, 0);
  if (total <= OUTPUT_MAX_CHARS) {
    return [...runs];
  }

  const kept = [...runs];
  while (kept.length > 1 && total - (kept[0]?.text.length ?? 0) > OUTPUT_MAX_CHARS) {
    total -= kept[0]?.text.length ?? 0;
    kept.shift();
  }

  const first = kept[0];
  if (first !== undefined && total > OUTPUT_MAX_CHARS) {
    kept[0] = { ...first, text: first.text.slice(total - OUTPUT_MAX_CHARS) };
  }
  return kept;
}

function ordered(sessions: readonly SessionSummary[]): SessionSummary[] {
  return [...sessions].sort(
    (a, b) =>
      b.updated_at.localeCompare(a.updated_at) ||
      b.created_at.localeCompare(a.created_at),
  );
}

export const useSessions = create<SessionsState>((set, get) => {
  /** Runs a command, holding any failure in `error` and clearing `busy`. */
  const guard = async <T>(
    command: string,
    run: () => Promise<T>,
  ): Promise<{ ok: true; value: T } | { ok: false }> => {
    set({ busy: true, error: null });
    try {
      return { ok: true, value: await run() };
    } catch (cause) {
      set({ error: toIpcError(cause, command) });
      return { ok: false };
    } finally {
      set({ busy: false });
    }
  };

  /**
   * Re-reads a session, replacing whatever was assembled locally.
   *
   * Used where an optimistic change has to be undone. The failure is dropped
   * rather than reported: the caller is already on an error path, and a second
   * banner about the refetch would bury the first.
   */
  const reconcile = async (sessionId: string): Promise<void> => {
    try {
      const detail = await sessionOpen(sessionId);
      if (get().detail?.session.id === sessionId) {
        set({ detail });
      }
    } catch {
      // Nothing to reconcile against; the session is gone.
    }
  };

  return {
    sessions: [],
    detail: null,
    streaming: null,
    output: {},
    busy: false,
    error: null,

    reset: () =>
      set({ sessions: [], detail: null, streaming: null, output: {}, error: null }),

    loadFor: async (projectId) => {
      const outcome = await guard("session_list", async () => {
        const sessions = await sessionList(projectId);
        const first = sessions.at(0);
        const detail = first === undefined ? null : await sessionOpen(first.id);
        return { sessions, detail };
      });

      if (outcome.ok) {
        set({ ...outcome.value, streaming: null, output: {} });
      }
    },

    open: async (sessionId) => {
      if (get().detail?.session.id === sessionId) {
        return;
      }
      const outcome = await guard("session_open", () => sessionOpen(sessionId));
      if (outcome.ok) {
        // The streaming buffer and the output panes belong to the session
        // being left, not to this one. A turn still running elsewhere keeps
        // going; its events are filtered out until the user comes back and
        // re-opens it — and what a command printed is not on disk, so coming
        // back shows the summary rather than the pane.
        set({ detail: outcome.value, streaming: null, output: {} });
      }
    },

    create: async (projectId) => {
      const outcome = await guard("session_create", async () => {
        const created = await sessionCreate(projectId);
        return {
          created,
          sessions: await sessionList(projectId),
          detail: await sessionOpen(created.id),
        };
      });

      if (outcome.ok) {
        set({
          sessions: outcome.value.sessions,
          detail: outcome.value.detail,
          streaming: null,
          output: {},
        });
      }
    },

    rename: async (sessionId, title) => {
      // The runtime rejects an empty title; not sending one at all is a
      // clearer no-op than a round trip that comes back as an error banner.
      if (title.trim() === "") {
        return;
      }
      await guard("session_rename", () => sessionRename(sessionId, title));
    },

    remove: async (sessionId) => {
      const outcome = await guard("session_delete", async () => {
        await sessionDelete(sessionId);
      });
      if (!outcome.ok) {
        return;
      }

      set((state) => {
        const sessions = state.sessions.filter((s) => s.id !== sessionId);
        const closing = state.detail?.session.id === sessionId;
        return {
          sessions,
          detail: closing ? null : state.detail,
          streaming: closing ? null : state.streaming,
          output: closing ? {} : state.output,
        };
      });

      // Deleting the open session leaves the pane empty; the next most recent
      // session is the one the user was working in before it.
      const next = get().sessions.at(0);
      if (get().detail === null && next !== undefined) {
        await get().open(next.id);
      }
    },

    send: async (text) => {
      const detail = get().detail;
      if (detail === null || text.trim() === "") {
        return;
      }
      const sessionId = detail.session.id;

      // Shown immediately, replaced by the real record when the turn is
      // reconciled. The id is local and never sent anywhere.
      const optimistic: Message = {
        id: `local-${Date.now()}`,
        role: "user",
        text,
        tool_calls: [],
        tool_call_id: null,
        created_at: new Date().toISOString(),
      };
      set((state) =>
        state.detail === null
          ? state
          : {
              detail: {
                ...state.detail,
                messages: [...state.detail.messages, optimistic],
              },
            },
      );

      const outcome = await guard("session_send", () =>
        sessionSend(sessionId, text),
      );

      // A rejected send leaves the optimistic message stranded. Reconciling
      // removes it, which is what makes `E_TURN_BUSY` recoverable: the text is
      // still in the composer, and nothing was added to the transcript.
      if (!outcome.ok) {
        await reconcile(sessionId);
      }
    },

    cancel: async () => {
      const { detail, streaming } = get();
      if (detail === null || streaming === null) {
        return;
      }
      await guard("session_cancel", () =>
        sessionCancel(detail.session.id, streaming.turnId),
      );
    },

    dismissError: () => set({ error: null }),
  };
});

/**
 * Attaches the store to the runtime's event stream.
 *
 * Called once, from the shell. Returns the detach function; a listener left
 * attached after unmount writes into a store nothing is rendering.
 */
export function attachSessionEvents(): Promise<() => void> {
  const { getState, setState } = useSessions;

  /** Whether an event belongs to the session currently on screen. */
  const isOpen = (sessionId: string) =>
    getState().detail?.session.id === sessionId;

  return subscribe({
    "turn:started": ({ session_id, turn_id, model }) => {
      if (isOpen(session_id)) {
        setState({ streaming: { turnId: turn_id, model, text: "", seq: -1 } });
      }
    },

    "turn:delta": ({ session_id, seq, text }) => {
      if (!isOpen(session_id)) {
        return;
      }
      setState((state) => {
        const streaming = state.streaming;
        // Out of order, or already applied. Dropping rather than appending is
        // what makes re-syncing after a reload safe.
        if (streaming === null || seq <= streaming.seq) {
          return state;
        }
        return { streaming: { ...streaming, seq, text: streaming.text + text } };
      });
    },

    "turn:message": ({ session_id, message }) => {
      if (!isOpen(session_id)) {
        return;
      }
      setState((state) =>
        state.detail === null
          ? state
          : {
              detail: {
                ...state.detail,
                messages: [...state.detail.messages, message],
              },
              // The buffer has been superseded by the finalized message. A
              // turn with tool calls streams again after this, into an empty
              // buffer.
              streaming:
                state.streaming === null
                  ? null
                  : { ...state.streaming, text: "" },
            },
      );
    },

    // Live output from a running command. Kept in the store rather than
    // reconciled from disk afterwards: the runtime records a one-line summary
    // and an audit line, never the transcript of a build.
    "tool:progress": ({ session_id, call_id, stream, seq, chunk }) => {
      if (!isOpen(session_id)) {
        return;
      }
      setState((state) => {
        const current = state.output[call_id];
        // Out of order, or already applied. Dropping rather than appending is
        // what makes a re-sync safe, exactly as for `turn:delta`.
        if (current !== undefined && seq <= current.seq) {
          return state;
        }
        return {
          output: {
            ...state.output,
            [call_id]: appended(current, stream, chunk, seq),
          },
        };
      });
    },

    "tool:finished": ({ session_id, call_id, summary, truncated }) => {
      if (!isOpen(session_id)) {
        return;
      }
      setState((state) => {
        const shown = state.output[call_id];

        return {
          // Only what the pane could not show is recorded here; the summary
          // and the audit line carry the rest.
          output:
            shown === undefined || !truncated
              ? state.output
              : { ...state.output, [call_id]: { ...shown, truncated: true } },
          detail:
            state.detail === null
              ? state.detail
              : {
                  ...state.detail,
                  messages: state.detail.messages.map((message) => ({
                    ...message,
                    tool_calls: message.tool_calls.map((call) =>
                      call.call_id === call_id ? { ...call, summary } : call,
                    ),
                  })),
                },
        };
      });
    },

    "turn:error": ({ session_id, code, message, retryable }) => {
      if (!isOpen(session_id)) {
        return;
      }
      setState({
        error: Object.assign(new Error(message), {
          name: "IpcError",
          code,
          retryable,
          command: "session_send",
        }) as IpcError,
      });
    },

    "turn:finished": ({ session_id }) => {
      if (!isOpen(session_id)) {
        return;
      }
      setState({ streaming: null });

      // The authoritative transcript: the `tool` messages the UI never saw
      // stream, and any repair the runtime made along the way. Fetched rather
      // than assembled, so what is on screen after a turn is what is on disk.
      void sessionOpen(session_id)
        .then((detail) => {
          // The user may have switched sessions while this was in flight.
          if (getState().detail?.session.id !== session_id) {
            return;
          }
          setState((state) => ({
            detail,
            sessions: ordered(
              state.sessions.map((session) =>
                session.id === session_id ? detail.session : session,
              ),
            ),
          }));
        })
        .catch(() => {
          // The session was deleted while its turn was finishing. The sidebar
          // already knows; there is nothing to reconcile against.
        });
    },

    // Every row, not only the open one: this is what keeps a running badge on
    // a session the user has navigated away from.
    "session:updated": (summary) => {
      setState((state) => {
        const known = state.sessions.some((session) => session.id === summary.id);
        const sessions = known
          ? ordered(
              state.sessions.map((session) =>
                session.id === summary.id ? summary : session,
              ),
            )
          : state.sessions;

        return {
          sessions,
          detail:
            state.detail?.session.id === summary.id
              ? { ...state.detail, session: summary }
              : state.detail,
        };
      });
    },
  });
}
