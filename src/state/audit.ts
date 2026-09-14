/**
 * The audit log, as the drawer sees it.
 *
 * A read-only window onto the log's tail; nothing in the WebView can write it.
 * Fetched only while the drawer is open; `audit:appended` prepends, anything
 * else (open, scope, session) refetches. Scope is a separate `audit_tail` call.
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

/** How many entries to fetch and keep (the runtime clamps at 1000). */
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
   * Reads the tail for the current scope and session, discarding the answer
   * if either changed meanwhile.
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

      // The path is fetched once; it exists even before the first line.
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
 * Subscribes the store to `audit:appended` (attached by `AppShell`); ignored
 * while the drawer is closed.
 */
export function attachAuditEvents(): Promise<UnlistenFn> {
  return subscribe({
    "audit:appended": (entry) => {
      useAudit.getState().applyAppended(entry);
    },
  });
}
