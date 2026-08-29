/**
 * Typed `listen` wrappers — one entry per event the runtime emits.
 *
 * Components and stores never call `listen` directly. The event name and its
 * payload type are paired exactly once, in {@link EventPayloads}, so a handler
 * that reads `payload.turn_id` on an event that has none is a compile error
 * rather than an `undefined` at runtime.
 *
 * Payload types are generated from the Rust structs into `./bindings.ts`; this
 * file only maps names to them.
 *
 * Two properties of the runtime side shape how these are used, and both are
 * documented in `PLAN.md` § 2.2:
 *
 * - Every payload carries `session_id`, so a listener showing one session can
 *   drop another's traffic without knowing what the event means.
 * - `seq` on `turn:delta` is a per-turn counter from zero. A listener that
 *   drops anything it has already seen re-syncs safely after a reload.
 */

import { listen } from "@tauri-apps/api/event";
import type { UnlistenFn } from "@tauri-apps/api/event";

import type {
  ApprovalRequest,
  AuditEntry,
  SessionSummary,
  ToolApprovalResolved,
  ToolFinished,
  ToolProgress,
  ToolRequested,
  ToolStarted,
  TrayActivate,
  TurnDelta,
  TurnError,
  TurnFinished,
  TurnMessage,
  TurnStarted,
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
   * An approval stopped being pending — by a click, a timeout, or a cancelled
   * turn. Emitted for every ending, so a card is never left on screen for a
   * call nothing will run. May arrive twice for one answer (once from the
   * command, once from the turn waking up); handlers key on `request_id` and
   * are idempotent.
   */
  "tool:approval_resolved": ToolApprovalResolved;
  /** A tool was cleared and is running. */
  "tool:started": ToolStarted;
  /**
   * Output from a tool that is still running — `shell_exec` only. Frames are
   * coalesced to ~50 ms and the total is capped, so this is a live view rather
   * than the record: `tool:finished` carrying `truncated` is what says the
   * pane stopped short of everything the command printed.
   */
  "tool:progress": ToolProgress;
  /** A tool ended, whatever became of it. */
  "tool:finished": ToolFinished;
  /** A session's sidebar row changed. */
  "session:updated": SessionSummary;
  /** A line was appended to the audit log. */
  "audit:appended": AuditEntry;
  /** The tray brought the window forward. */
  "tray:activate": TrayActivate;
};

/** Any event name the runtime emits. */
export type EventName = keyof EventPayloads;

/**
 * Subscribes to one event.
 *
 * Resolves to the function that cancels the subscription. Callers that mount
 * and unmount must await it and call the result — a listener left attached
 * outlives the component and writes into a store nothing is rendering.
 */
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
 * Subscribes to several events at once.
 *
 * Returns a single function that detaches all of them. Subscriptions are
 * established concurrently but the caller may unmount before they resolve, so
 * the returned function is safe to call at any point: listeners that arrive
 * after it are detached immediately rather than leaked.
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
