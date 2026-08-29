/**
 * The approval prompt: the one place a person says yes or no.
 *
 * Rendered inline above the composer rather than as a modal overlay. A turn
 * blocked on an approval is not a blocked application — the user may want to
 * scroll the transcript, read what the model said, or look at another session
 * before deciding, and a modal takes all of that away to prevent a mistake it
 * does not actually prevent.
 *
 * What the prompt has to make true is smaller and stricter: the user reads the
 * exact thing that would happen before it happens (PLAN 3.3). So the path, the
 * arguments and the pending content come first, the buttons are last, and none
 * of the three answers is the default — there is no auto-focused Allow, and
 * pressing Enter in the composer does not approve anything.
 *
 * "Allow for this session" appears only when the runtime says the row offers a
 * grant, and it is labelled with what it would actually cover, in the runtime's
 * own words. The rule is enforced in Rust either way (PLAN 3.1); hiding the
 * button is how the interface tells the truth, not how the rule holds.
 */

import type { ApprovalRequest, Decision } from "../../ipc/bindings";
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
