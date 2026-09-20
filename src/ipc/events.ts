/**
 * Typed `listen` wrappers — one entry per event the runtime emits.
 *
 * Names are paired with generated payload types once, in
 * {@link EventPayloads}. Every payload carries `session_id`; `seq` counts
 * per turn from zero (PLAN 2.2).
 */

import { listen } from "@tauri-apps/api/event";
import type { UnlistenFn } from "@tauri-apps/api/event";

import type {
  ApprovalRequest,
  AuditEntry,
  ConnectorView,
  MaskedSettings,
  ParkedAsk,
  ParkedResolved,
  Routine,
  SessionSummary,
  ToolApprovalAnnotated,
  ToolApprovalResolved,
  ToolFinished,
  ToolDrafting,
  ToolProgress,
  ToolRequested,
  ToolStarted,
  TrayActivate,
  TurnDelta,
  TurnError,
  TurnFinished,
  TurnMessage,
  TurnStarted,
  WorkspaceDropped,
} from "./bindings";

/**
 * Every event name, and what it carries.
 *
 * Names are `domain:verb` in past tense, matching the Rust constants in
 * `agent/event.rs`.
 */
export type EventPayloads = {
  /** A turn began. The composer locks until `turn:finished`. */
  "turn:started": TurnStarted;
  /** A frame of assistant text, already coalesced to ~50 ms. */
  "turn:delta": TurnDelta;
  /** An assistant message was finalized and written to disk. */
  "turn:message": TurnMessage;
  /** A turn ended, for any reason including failure. */
  "turn:finished": TurnFinished;
  /** A turn failed. Always followed by `turn:finished`. */
  "turn:error": TurnError;
  /** The model asked for a tool, before policy judged it. */
  "tool:requested": ToolRequested;
  /** A tool call is blocked on a human. The turn is parked until it is answered. */
  "tool:approval_required": ApprovalRequest;
  /**
   * The decision model annotated a pending approval (PLAN 7.18). Advisory;
   * the buttons do not change.
   */
  "tool:approval_annotated": ToolApprovalAnnotated;
  /**
   * An approval stopped being pending, however it ended. May arrive twice;
   * handlers key on `request_id`.
   */
  "tool:approval_resolved": ToolApprovalResolved;
  /** A tool was cleared and is running. */
  "tool:started": ToolStarted;
  /**
   * Bytes of tool-call arguments written so far, so a long `fs_write` does not
   * look hung. Coalesced like `turn:delta`.
   */
  "tool:drafting": ToolDrafting;
  /**
   * Live `shell_exec` output, coalesced and capped; `tool:finished.truncated`
   * says it stopped short.
   */
  "tool:progress": ToolProgress;
  /** A tool ended, whatever became of it. */
  "tool:finished": ToolFinished;
  /** A session's sidebar row changed. */
  "session:updated": SessionSummary;
  /** A line was appended to the audit log. */
  "audit:appended": AuditEntry;
  /** Provider settings changed; the same masked payload as `settings_get`. */
  "settings:changed": MaskedSettings;
  /** A routine's row changed: a run ended or it paused itself (Phase 16). */
  "routine:updated": Routine;
  /**
   * A call was parked for a person (PLAN 7.22): the run has ended and the
   * question is on the board until somebody answers it.
   */
  "parked:updated": ParkedAsk;
  /**
   * A parked ask stopped being open — answered, or out of time. Handlers key
   * on `id`.
   */
  "parked:resolved": ParkedResolved;
  /** A connector's row changed: up, new tools, or exited (Phase 18). */
  "connector:updated": ConnectorView;
  /**
   * The OS dropped files on the window (PLAN 7.15). Names, an id and where it
   * landed, in physical pixels — never the paths' bytes, and never a path the
   * window is expected to send back: `workspace_import_brief` takes the id.
   */
  "workspace:dropped": WorkspaceDropped;
  /** The tray brought the window forward. */
  "tray:activate": TrayActivate;
};

/** Any event name the runtime emits. */
export type EventName = keyof EventPayloads;

/** Subscribes to one event; resolves to the unsubscribe function. */
export function on<K extends EventName>(
  name: K,
  handler: (payload: EventPayloads[K]) => void,
): Promise<UnlistenFn> {
  return listen<EventPayloads[K]>(name, (event) => {
    handler(event.payload);
  });
}

/** One entry of a {@link subscribe} map: a name and its handler. */
export type Handlers = {
  readonly [K in EventName]?: (payload: EventPayloads[K]) => void;
};

/**
 * Subscribes to several events; the returned detach is safe to call before
 * they resolve.
 */
export async function subscribe(handlers: Handlers): Promise<UnlistenFn> {
  let cancelled = false;
  const attached: UnlistenFn[] = [];

  const detach = () => {
    cancelled = true;
    while (attached.length > 0) {
      attached.pop()?.();
    }
  };

  const pending = Object.entries(handlers).map(async ([name, handler]) => {
    // The map type guarantees the pairing; `Object.entries` erases it, and
    // this is the one place that has to assert what the type system knew.
    const unlisten = await on(
      name as EventName,
      handler as (payload: EventPayloads[EventName]) => void,
    );
    if (cancelled) {
      unlisten();
    } else {
      attached.push(unlisten);
    }
  });

  await Promise.all(pending);
  return detach;
}
