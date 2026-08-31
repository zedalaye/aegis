/**
 * One line of the audit log.
 *
 * The row answers, in order, the four questions someone opening the drawer
 * has: *when*, *what tool*, *did it run*, and *how did it end*. A fifth sits
 * beside the tool when there is one — which skill the call was part of, since
 * that is what turns a column of verbs back into a procedure. Everything else
 * — the ids, the identity it ran as, the delegation it belonged to, the digest,
 * the file a capture left behind — is behind a disclosure, because a drawer
 * that made every row six lines tall would be a drawer nobody scrolls.
 *
 * Two things this deliberately does not do.
 *
 * **It does not re-render the arguments as anything but text.** They are the
 * model's own output, already redacted by the runtime, and they go into a
 * `<pre>` — the WebView holds the whole UI, and a log viewer is the last place
 * that should be interpreting what it displays.
 *
 * **It does not restate the decision as an outcome when they are the same
 * fact.** A refused call always ends "never ran"; showing both chips would
 * read as two findings where there is one.
 */

import { useState } from "react";

import type { AuditDecision, AuditEntry, Outcome } from "../../ipc/bindings";
import {
  formatBytes,
  formatDuration,
  formatTimeOfDay,
  formatTimestamp,
} from "../../lib/format";

/** How a call came to run, in the words the approval dialog used. */
const DECISION: Record<AuditDecision, string> = {
  auto: "auto",
  allow_once: "allowed once",
  allow_session: "allowed for the session",
  deny: "refused",
};

/** How a call ended. */
const OUTCOME: Record<Outcome, string> = {
  ok: "done",
  error: "failed",
  denied: "never ran",
  cancelled: "cancelled",
};

/** Pretty-prints the redacted arguments, falling back to the raw string. */
function formatArgs(argsJson: string): string {
  try {
    return JSON.stringify(JSON.parse(argsJson) as unknown, null, 2);
  } catch {
    // The line on disk holds whatever the writer put there. Showing it
    // verbatim is more honest than showing nothing.
    return argsJson;
  }
}

/** A short prefix of a hex digest — enough to compare two rows by eye. */
function shortDigest(digest: string): string {
  return digest.length > 12 ? `${digest.slice(0, 12)}…` : digest;
}

export default function AuditRow({
  entry,
  showSession,
}: {
  readonly entry: AuditEntry;
  /** Whether to name the session — only useful when the list spans several. */
  readonly showSession: boolean;
}) {
  const [expanded, setExpanded] = useState(false);

  const refused = entry.decision === "deny";
  const failed = entry.outcome === "error";

  // The outcome class marks a refused or failed call down the left edge. The
  // chips below say the same thing in words — the stripe is for scanning, and
  // is never the only place the outcome appears.
  return (
    <li className={`auditrow auditrow--${entry.outcome}`}>
      <div className="auditrow__head">
        <time
          className="auditrow__time"
          dateTime={entry.ts}
          title={formatTimestamp(entry.ts)}
        >
          {formatTimeOfDay(entry.ts)}
        </time>
        <span className="auditrow__tool">{entry.tool}</span>
        {/* On the head rather than behind the disclosure: which runbook a call
            belongs to is what makes a column of tool names readable as a
            procedure instead of as a list of verbs. */}
        {entry.skill === "" ? null : (
          <span className="auditrow__skill" title={`Part of the ${entry.skill} run`}>
            {entry.skill}
          </span>
        )}
        <span className="auditrow__chips">
          {refused ? (
            <span className="auditrow__chip auditrow__chip--deny">
              {DECISION.deny}
            </span>
          ) : (
            <>
              <span className="auditrow__chip">
                {DECISION[entry.decision]}
              </span>
              <span
                className={`auditrow__chip auditrow__chip--${entry.outcome}`}
              >
                {OUTCOME[entry.outcome]}
              </span>
            </>
          )}
        </span>
      </div>

      <p className="auditrow__args" title={entry.args_redacted}>
        {entry.args_redacted}
      </p>

      <p className="auditrow__reason">{entry.policy_reason}</p>

      <div className="auditrow__meta">
        <span>{formatDuration(entry.duration_ms)}</span>
        <span>in {formatBytes(entry.bytes_in)}</span>
        <span>out {formatBytes(entry.bytes_out)}</span>
        {entry.error_code === null ? null : (
          <span className="auditrow__code">{entry.error_code}</span>
        )}
        <button
          type="button"
          className="link auditrow__more"
          aria-expanded={expanded}
          onClick={() => setExpanded((showing) => !showing)}
        >
          {expanded ? "Less" : "More"}
        </button>
      </div>

      {expanded ? (
        <div className="auditrow__detail">
          <dl className="facts">
            {showSession ? (
              <>
                <dt>Session</dt>
                <dd className="facts__path">{entry.session_id}</dd>
              </>
            ) : null}
            {/* Empty on a line written before identities existed. Drawn as
                nothing rather than as "default", because a line from an
                earlier build did not record one and saying it did would be
                inventing the record. */}
            {entry.agent_id === "" ? null : (
              <>
                <dt>Identity</dt>
                <dd className="facts__path">{entry.agent_id}</dd>
              </>
            )}
            {/* The one id that spans several sessions: a delegation covers the
                Chief of Staff's call and every specialist's turn under it, so
                filtering the log by it is how a whole run is read back
                (PLAN 7.2, row 10). */}
            {entry.handoff === "" ? null : (
              <>
                <dt>Delegation</dt>
                <dd className="facts__path">{entry.handoff}</dd>
              </>
            )}
            <dt>Turn</dt>
            <dd className="facts__path">{entry.turn_id}</dd>
            <dt>Call</dt>
            <dd className="facts__path">{entry.call_id}</dd>
            <dt>Arguments</dt>
            <dd>
              <pre className="auditrow__json">
                {formatArgs(entry.args_redacted)}
              </pre>
            </dd>
            <dt title={entry.args_digest}>Digest</dt>
            <dd className="facts__path">{shortDigest(entry.args_digest)}</dd>
            {entry.artifact === null ? null : (
              <>
                <dt>Wrote</dt>
                <dd className="facts__path">
                  {entry.artifact.path}
                  <br />
                  {entry.artifact.width}×{entry.artifact.height} ·{" "}
                  {shortDigest(entry.artifact.sha256)}
                </dd>
              </>
            )}
          </dl>
          <p className="auditrow__note">
            {failed
              ? "The tool ran and failed on its own terms. The line above is the whole record; nothing of what it read or wrote is kept here."
              : "Arguments are the ones the model sent, shortened — file content is replaced by its size, never quoted. The digest is over them in full."}
          </p>
        </div>
      ) : null}
    </li>
  );
}
