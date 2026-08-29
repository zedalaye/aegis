/**
 * The audit log, as the drawer sees it.
 *
 * The log on disk is the record; this store is a window onto its tail. It
 * holds no truth of its own and there is no command anywhere in the WebView
 * that writes, edits or clears a line — a UI able to do that would be a UI
 * able to forge the record of what the agent did.
 *
 * Three decisions shape it.
 *
 * **It only reads while the drawer is open.** A tail on every session switch
 * would be a file read nobody asked for, and the log can be months long. Open
 * fetches; closed forgets. That also makes the reconciliation trivial: there is
 * no stale cache to repair, because there is no cache when nothing is looking.
 *
 * **Events extend, the command establishes.** `audit:appended` prepends the
 * line the runtime has just written, so a call approved with the drawer open
 * appears as it happens rather than on the next refresh. Everything else —
 * opening, switching scope, switching session — refetches, because the event
 * stream only covers the time the window was listening.
 *
 * **Scope is a filter on what to ask for, not on what was kept.** "This
 * session" and "everything" are two different `audit_tail` calls; the store
 * never holds one and renders the other, so what is on screen is always a list
 * the runtime produced.
 */

import { create } from "zustand";

import type { AuditEntry } from "../ipc/bindings";
import { auditLogPath, auditTail } from "../ipc/commands";
import { subscribe } from "../ipc/events";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

/** Whether the log has been read yet. */
export type LoadStatus = "idle" | "loading" | "ready" | "error";

/** Which calls the drawer is showing. */
export type AuditScope = "session" | "all";

/**
 * How many entries to ask for, and to keep.
 *
 * Rust clamps `audit_tail` at 1000; this is well under it. The drawer is for
 * "what has this agent been doing", which is answered by the last couple of
 * hundred calls — and a list bounded here is a list that cannot grow without
 * limit as a long-running session appends to it.
 */
const TAIL_LIMIT = 200;

export type AuditState = {
  /** The tail, newest first. */
  readonly entries: readonly AuditEntry[];
  readonly status: LoadStatus;
  /** Whether the drawer is showing. */
  readonly open: boolean;
  /** Which calls it is showing. */
  readonly scope: AuditScope;
  /** The open session, which `scope: "session"` narrows to. */
  readonly sessionId: string | null;
  /** Where the log lives, once asked for. */
  readonly logPath: string | null;
  /** The last failure, or `null`. */
  readonly error: IpcError | null;

  /** Shows the drawer and reads the log. */
  openDrawer: () => Promise<void>;
  /** Hides the drawer and drops what it was showing. */
  closeDrawer: () => void;
  /** Opens or closes, whichever it is not. */
  toggleDrawer: () => Promise<void>;
  /** Changes what the drawer is showing, and refetches. */
  setScope: (scope: AuditScope) => Promise<void>;
  /** Points the store at the open session, refetching if that changed. */
  followSession: (sessionId: string | null) => Promise<void>;
  /** Re-reads the tail. */
  refresh: () => Promise<void>;
  /** Applies an `audit:appended` payload. */
  applyAppended: (entry: AuditEntry) => void;
  /** Clears the last error. */
  dismissError: () => void;
};

export const useAudit = create<AuditState>((set, get) => {
  /**
   * Reads the tail for whatever scope and session are current.
   *
   * The scope and session are captured before the call and checked after it:
   * a user who switches scope while a read is in flight must not be shown the
   * answer to the question they have just stopped asking.
   */
  const read = async (): Promise<void> => {
    const { scope, sessionId } = get();

    // "This session" with no session open is an empty list rather than the
    // whole log — a scope that silently widened when nothing was selected
    // would show other projects' calls under a label saying it does not.
    if (scope === "session" && sessionId === null) {
      set({ entries: [], status: "ready" });
      return;
    }

    set({ status: "loading", error: null });
    try {
      const entries = await auditTail(
        TAIL_LIMIT,
        scope === "session" ? (sessionId ?? undefined) : undefined,
      );
      const current = get();
      if (current.scope === scope && current.sessionId === sessionId) {
        set({ entries, status: "ready" });
      }
    } catch (cause) {
      set({ error: toIpcError(cause, "audit_tail"), status: "error" });
    }
  };

  return {
    entries: [],
    status: "idle",
    open: false,
    scope: "session",
    sessionId: null,
    logPath: null,
    error: null,

    openDrawer: async () => {
      set({ open: true });

      // The path is a fact about the installation, not about the log's
      // contents, so it is fetched once and kept. It is returned even before
      // the first line is written — the drawer's empty state says where the
      // file *will* be, which is what someone checking "is anything being
      // recorded" needs.
      if (get().logPath === null) {
        try {
          set({ logPath: await auditLogPath() });
        } catch (cause) {
          // Not worth a banner: the list below is the point, and a missing
          // footer path is a smaller loss than an alert over the window.
          set({ error: toIpcError(cause, "audit_log_path") });
        }
      }

      await read();
    },

    closeDrawer: () => set({ open: false, entries: [], status: "idle" }),

    toggleDrawer: async () => {
      if (get().open) {
        get().closeDrawer();
      } else {
        await get().openDrawer();
      }
    },

    setScope: async (scope) => {
      if (get().scope === scope) {
        return;
      }
      set({ scope, entries: [] });
      if (get().open) {
        await read();
      }
    },

    followSession: async (sessionId) => {
      if (get().sessionId === sessionId) {
        return;
      }
      set({ sessionId });

      // Only the session scope is looking at the session. Refetching under
      // "everything" would replace an identical list with itself.
      if (get().open && get().scope === "session") {
        set({ entries: [] });
        await read();
      }
    },

    refresh: async () => {
      await read();
    },

    applyAppended: (entry) => {
      const { open, scope, sessionId, entries } = get();
      if (!open) {
        return;
      }
      if (scope === "session" && entry.session_id !== sessionId) {
        return;
      }

      // Keyed on the call id and the timestamp together: a call id is the
      // model's, and a model that reuses one across turns must not silence the
      // second line. The pair is what the log itself would show as two rows.
      const known = entries.some(
        (held) => held.call_id === entry.call_id && held.ts === entry.ts,
      );
      if (known) {
        return;
      }

      set({
        entries: [entry, ...entries].slice(0, TAIL_LIMIT),
        status: "ready",
      });
    },

    dismissError: () => set({ error: null }),
  };
});

/**
 * Subscribes the store to `audit:appended`.
 *
 * One listener for the app, attached by `AppShell` beside the others. The
 * handler drops everything while the drawer is closed — opening it refetches,
 * so a line missed here is a line that will be read from the file a moment
 * later, and the alternative is keeping a list in memory for a panel nobody is
 * looking at.
 */
export function attachAuditEvents(): Promise<UnlistenFn> {
  return subscribe({
    "audit:appended": (entry) => {
      useAudit.getState().applyAppended(entry);
    },
  });
}
