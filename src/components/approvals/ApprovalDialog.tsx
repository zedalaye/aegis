/**
 * The approval prompt: the one place a person says yes or no.
 *
 * Inline above the composer, not a modal. Details first, buttons last, and no
 * default answer (PLAN 3.3). "Allow for this session" appears only when the
 * runtime offers a grant, labelled with its scope (enforced in Rust, PLAN 3.1).
 */

import type {
  ApprovalRequest,
  Decision,
  RiskAnnotation,
} from "../../ipc/bindings";
import { useApprovals } from "../../state/approvals";

import DiffPreview from "./DiffPreview";
import RiskBadge from "./RiskBadge";

/** How many more are queued behind this one. */
function Queue({ count }: { readonly count: number }) {
  if (count <= 0) {
    return null;
  }
  return (
    <span className="approval__queue">
      {count} more waiting
    </span>
  );
}

/**
 * What the decision model said (PLAN 7.18). Advisory: it changes the words
 * shown, never the buttons.
 */
function Annotation({ annotation }: { readonly annotation: RiskAnnotation }) {
  const facts = [
    `destructive ${annotation.destructive}%`,
    `sends content out ${annotation.exfil}%`,
    `changes git ${annotation.git_history}%`,
  ];
  if (annotation.undo !== null) {
    facts.push(`hard to undo ${annotation.undo}%`);
  }
  if (annotation.bucket !== null) {
    facts.push(annotation.bucket.replace(/_/g, " "));
  }
  return (
    <p
      className={`approval__annotation${annotation.raised ? " approval__annotation--raised" : ""}`}
      role="note"
    >
      {annotation.summary}{" "}
      <span className="approval__annotation-facts">
        {facts.join(" · ")}
        {annotation.model.length > 0 ? ` — ${annotation.model}` : ""}
      </span>
    </p>
  );
}

export default function ApprovalDialog({
  request,
  queued = 0,
}: {
  readonly request: ApprovalRequest;
  /** How many other approvals are behind this one. */
  readonly queued?: number;
}) {
  const resolve = useApprovals((s) => s.resolve);
  const busy = useApprovals((s) =>
    s.resolving.includes(request.request_id),
  );

  const answer = (decision: Decision) => {
    void resolve(request.request_id, decision);
  };

  return (
    <section
      className={`approval approval--${request.risk}`}
      role="alertdialog"
      aria-labelledby={`approval-title-${request.request_id}`}
    >
      <header className="approval__header">
        <h2
          className="approval__title"
          id={`approval-title-${request.request_id}`}
        >
          {request.title}
        </h2>
        <RiskBadge risk={request.risk} />
        <Queue count={queued} />
      </header>

      <p className="approval__summary">{request.summary}</p>
      <p className="approval__reason">Aegis is asking because {request.reason}.</p>
      {request.annotation === null ? null : (
        <Annotation annotation={request.annotation} />
      )}

      <DiffPreview detail={request.detail} />

      <div className="approval__actions">
        <button
          type="button"
          className="button button--danger"
          disabled={busy}
          onClick={() => answer("deny")}
        >
          Deny
        </button>
        <button
          type="button"
          className="button"
          disabled={busy}
          onClick={() => answer("allow_once")}
        >
          Allow once
        </button>
        {request.session_grant_allowed ? (
          <button
            type="button"
            className="button button--primary"
            disabled={busy}
            onClick={() => answer("allow_session")}
            title={`This would ${request.scope_label}.`}
          >
            Allow for this session
          </button>
        ) : null}
      </div>

      <p className="approval__scope">
        {request.session_grant_allowed
          ? `“Allow for this session” would ${request.scope_label}. You can take it back at any time.`
          : "This one cannot be allowed for the whole session — you will be asked again next time."}
      </p>
    </section>
  );
}
