/**
 * The pending approval queue, and the grants answering one can create.
 *
 * - Events add and remove cards; {@link ApprovalsState.syncFor} refetches the
 *   runtime's queue when the open session changes.
 * - `E_APPROVAL_STALE` drops the card and re-syncs.
 * - `allow_session` is hidden when `session_grant_allowed` is false; the
 *   runtime enforces it anyway (PLAN 3.1).
 */

import { create } from "zustand";

import type { ApprovalRequest, Decision, Grant } from "../ipc/bindings";
import { sameGrant } from "../lib/grants";
import {
  approvalGrants,
  approvalListPending,
  approvalResolve,
  approvalRevokeGrant,
} from "../ipc/commands";
import { subscribe } from "../ipc/events";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

export type ApprovalsState = {
  /** What the open session is blocked on, oldest first. */
  readonly pending: readonly ApprovalRequest[];
  /** The `allow_session` grants the open session holds. */
  readonly grants: readonly Grant[];
  /** The session these belong to, or `null` when none is open. */
  readonly sessionId: string | null;
  /** Request ids currently being answered, so a button cannot be double-clicked. */
  readonly resolving: readonly string[];
  /** The last failure, or `null`. */
  readonly error: IpcError | null;

  /** Points the store at a session and refetches both lists. */
  syncFor: (sessionId: string | null) => Promise<void>;
  /** Answers one approval. */
  resolve: (requestId: string, decision: Decision) => Promise<void>;
  /** Withdraws one grant, so its tool is asked about again. */
  revoke: (grant: Grant) => Promise<void>;
  /** Clears the last error. */
  dismissError: () => void;
};

export const useApprovals = create<ApprovalsState>((set, get) => {
  /** Refetches both lists for whichever session is open. */
  const reload = async (sessionId: string): Promise<void> => {
    const [pending, grants] = await Promise.all([
      approvalListPending(sessionId),
      approvalGrants(sessionId),
    ]);
    // The user may have switched sessions while this was in flight; applying
    // it then would show one session's dialogs above another's transcript.
    if (get().sessionId === sessionId) {
      set({ pending, grants });
    }
  };

  return {
    pending: [],
    grants: [],
    sessionId: null,
    resolving: [],
    error: null,

    syncFor: async (sessionId) => {
      if (sessionId === null) {
        set({ sessionId: null, pending: [], grants: [], resolving: [] });
        return;
      }

      set({ sessionId, pending: [], grants: [], resolving: [] });
      try {
        await reload(sessionId);
      } catch (cause) {
        set({ error: toIpcError(cause, "approval_list_pending") });
      }
    },

    resolve: async (requestId, decision) => {
      const { sessionId, resolving } = get();
      if (sessionId === null || resolving.includes(requestId)) {
        return;
      }

      set({ resolving: [...resolving, requestId], error: null });
      try {
        await approvalResolve(requestId, decision);

        // Removed here as well as on the event: the turn's own
        // `tool:approval_resolved` is authoritative but arrives when its task
        // is next scheduled, and a card that lingers after a click reads as a
        // click that did not register.
        set((state) => ({
          pending: state.pending.filter(
            (request) => request.request_id !== requestId,
          ),
        }));

        // An `allow_session` answer created a grant; nothing else did.
        if (decision === "allow_session") {
          await reload(sessionId);
        }
      } catch (cause) {
        const error = toIpcError(cause, "approval_resolve");
        set({ error });

        // The click did nothing. Drop the card and re-sync rather than leave
        // one the runtime does not know about — except for a refused
        // `allow_session`, where the request is deliberately still open and
        // the user can answer it another way.
        if (error.code !== "E_GRANT_NOT_ALLOWED") {
          set((state) => ({
            pending: state.pending.filter(
              (request) => request.request_id !== requestId,
            ),
          }));
          await reload(sessionId).catch(() => {
            // Nothing to reconcile against; the session is gone.
          });
        }
      } finally {
        set((state) => ({
          resolving: state.resolving.filter((id) => id !== requestId),
        }));
      }
    },

    revoke: async (grant) => {
      const sessionId = get().sessionId;
      if (sessionId === null) {
        return;
      }

      set({ error: null });
      try {
        await approvalRevokeGrant(sessionId, grant);
        set((state) => ({
          grants: state.grants.filter((held) => !sameGrant(held, grant)),
        }));
      } catch (cause) {
        set({ error: toIpcError(cause, "approval_revoke_grant") });
      }
    },

    dismissError: () => set({ error: null }),
  };
});

/**
 * Attaches the store to the runtime's event stream.
 *
 * Called once, from the shell, beside the session subscription. Returns the
 * detach function.
 */
export function attachApprovalEvents(): Promise<() => void> {
  const { getState, setState } = useApprovals;

  /** Whether an event belongs to the session currently on screen. */
  const isOpen = (sessionId: string) => getState().sessionId === sessionId;

  return subscribe({
    "tool:approval_required": (request) => {
      if (!isOpen(request.session_id)) {
        return;
      }
      setState((state) => {
        // A re-sync may have queued this already; the runtime is the only
        // source of request ids, so the id is enough to tell.
        const known = state.pending.some(
          (queued) => queued.request_id === request.request_id,
        );
        return known ? state : { pending: [...state.pending, request] };
      });
    },

    // Arrives after the card, if at all; a card already answered is gone.
    "tool:approval_annotated": ({ session_id, request_id, annotation }) => {
      if (!isOpen(session_id)) {
        return;
      }
      setState((state) => ({
        pending: state.pending.map((request) =>
          request.request_id === request_id
            ? { ...request, annotation }
            : request,
        ),
      }));
    },

    // Fires for every ending, including the ones nobody clicked: a timeout, a
    // cancelled turn, a deleted session. Idempotent, because the click that
    // caused it has usually already removed the card.
    "tool:approval_resolved": ({ session_id, request_id }) => {
      if (!isOpen(session_id)) {
        return;
      }
      setState((state) => ({
        pending: state.pending.filter(
          (request) => request.request_id !== request_id,
        ),
      }));
    },

    // A grant created by an `allow_session` in *this* window is applied by the
    // command; one created while a turn was running still shows up here,
    // because the turn re-reads the session row when it resumes.
    "session:updated": ({ id, state }) => {
      if (!isOpen(id) || state !== "idle") {
        return;
      }
      const sessionId = getState().sessionId;
      if (sessionId === null) {
        return;
      }
      void approvalGrants(sessionId)
        .then((grants) => {
          if (getState().sessionId === sessionId) {
            setState({ grants });
          }
        })
        .catch(() => {
          // The session was deleted; its grants went with it.
        });
    },
  });
}
