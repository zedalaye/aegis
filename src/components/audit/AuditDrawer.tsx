/**
 * The audit drawer: what the agent has actually done, from the file that
 * recorded it.
 *
 * A panel beside the work area rather than over it. The transcript is the
 * story the model tells about a session; this is the record kept independently
 * of it, and the two are worth reading side by side — a call that appears in
 * one and not the other is exactly the thing a user opens this to find.
 *
 * It reads the log rather than the transcript on purpose. The lines are on
 * disk as JSONL whether or not Aegis is running, they cover sessions that have
 * since been deleted, and they are written for every call including the
 * refused ones. That independence is the whole value: a record that came from
 * the same in-memory state as the chat could not contradict it.
 *
 * Scope has two settings and no third. "This session" is the common question —
 * what did the conversation in front of me do — and "Everything" is the other
 * one, what has this machine's agent done at all. There is no filter by tool
 * or by outcome: the list is short, bounded and searchable by eye, and the
 * file is there for anything more.
 */

import { useEffect } from "react";

import { useAudit } from "../../state/audit";
import type { AuditScope } from "../../state/audit";
import { useSessions } from "../../state/sessions";

import AuditRow from "./AuditRow";

/** The two scopes, and what each is called. */
const SCOPES: readonly (readonly [AuditScope, string])[] = [
  ["session", "This session"],
  ["all", "Everything"],
];

/**
 * What the drawer says when it has nothing to list.
 *
 * Two different nothings, and the fix differs: no session is selected, or this
 * agent has genuinely not run a tool. An empty list that did not say which
 * would read as a log that is not recording.
 */
function Empty() {
  const scope = useAudit((s) => s.scope);
  const sessionId = useAudit((s) => s.sessionId);

  if (scope === "session" && sessionId === null) {
    return (
      <p className="drawer__empty">
        No session is open. Pick one to see the calls it made, or switch to{" "}
        <em>Everything</em> for the whole log.
      </p>
    );
  }

  return (
    <p className="drawer__empty">
      No tool calls recorded{scope === "session" ? " in this session" : ""} yet.
      A line lands here for every call the moment it is decided — allowed,
      refused or failed alike — and the file below is created by the first one.
    </p>
  );
}

export default function AuditDrawer() {
  const open = useAudit((s) => s.open);
  const entries = useAudit((s) => s.entries);
  const status = useAudit((s) => s.status);
  const scope = useAudit((s) => s.scope);
  const logPath = useAudit((s) => s.logPath);
  const error = useAudit((s) => s.error);
  const close = useAudit((s) => s.closeDrawer);
  const setScope = useAudit((s) => s.setScope);
  const refresh = useAudit((s) => s.refresh);
  const followSession = useAudit((s) => s.followSession);

  const sessionId = useSessions((s) => s.detail?.session.id ?? null);

  // The drawer follows the open session the way the approval queue does. The
  // store decides whether that costs a read: under "Everything" the list does
  // not depend on which session is in front of the user.
  useEffect(() => {
    void followSession(sessionId);
  }, [sessionId, followSession]);

  if (!open) {
    return null;
  }

  return (
    <aside className="drawer" aria-labelledby="audit-title">
      <header className="drawer__header">
        <h2 className="drawer__title" id="audit-title">
          Audit log
        </h2>
        <button type="button" className="button" onClick={close}>
          Close
        </button>
      </header>

      <div className="drawer__controls">
        <div
          className="drawer__scopes"
          role="group"
          aria-label="Which calls to show"
        >
          {SCOPES.map(([value, label]) => (
            <button
              key={value}
              type="button"
              className={`drawer__scope${scope === value ? " drawer__scope--on" : ""}`}
              aria-pressed={scope === value}
              onClick={() => void setScope(value)}
            >
              {label}
            </button>
          ))}
        </div>
        <button
          type="button"
          className="link"
          disabled={status === "loading"}
          onClick={() => void refresh()}
        >
          {status === "loading" ? "Reading…" : "Refresh"}
        </button>
      </div>

      {error === null ? null : (
        <p className="drawer__error" role="alert">
          {error.message} <span className="banner__code">{error.code}</span>
        </p>
      )}

      {entries.length === 0 ? (
        status === "loading" ? (
          <p className="drawer__empty">Reading the log…</p>
        ) : (
          <Empty />
        )
      ) : (
        <ol className="drawer__list">
          {entries.map((entry) => (
            <AuditRow
              // A call id is the model's and a timestamp is the runtime's;
              // together they are what makes two lines two lines.
              key={`${entry.ts}:${entry.call_id}`}
              entry={entry}
              showSession={scope === "all"}
            />
          ))}
        </ol>
      )}

      {logPath === null ? null : (
        <footer className="drawer__footer">
          <p className="drawer__path" title={logPath}>
            {logPath}
          </p>
          <p className="drawer__note">
            Plain JSONL, one line per call, append-only. Nothing in this window
            can write to it or clear it.
          </p>
        </footer>
      )}
    </aside>
  );
}
