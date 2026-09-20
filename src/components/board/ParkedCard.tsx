/**
 * One call a run parked, and the three answers it takes (PLAN 7.22).
 *
 * The same reading as the approval dialog — details first, buttons last, no
 * default answer (PLAN 3.3) — with one difference that matters: nothing is
 * waiting on this. The run has already ended, and an answer starts it again.
 */

import type { Decision, ParkedAsk } from "../../ipc/bindings";
import { formatTimestamp } from "../../lib/format";

import DiffPreview from "../approvals/DiffPreview";
import RiskBadge from "../approvals/RiskBadge";

/** What each answer does, in the words on the button's title. */
const ANSWERS: Record<Decision, string> = {
  allow_once: "Runs this one call, exactly as it is written here, and nothing else.",
  allow_session: "Signs a standing approval, so this routine stops asking for what it covers.",
  deny: "Refuses it. The run is picked up again and told not to look for another way.",
};

export default function ParkedCard({
  ask,
  busy,
  onAnswer,
}: {
  readonly ask: ParkedAsk;
  /** True while this card's answer is in flight. */
  readonly busy: boolean;
  readonly onAnswer: (parkedId: string, decision: Decision) => void;
}) {
  const standing = ask.grant !== null;
  const who =
    ask.routine_name.length > 0
      ? `${ask.routine_name} parked this while nobody was watching.`
      : "This dialog was left unanswered, so the call was kept rather than thrown away.";

  return (
    <section className={`parked parked--${ask.risk}`} aria-labelledby={`parked-${ask.id}`}>
      <header className="parked__header">
        <h3 className="parked__title" id={`parked-${ask.id}`}>
          {ask.title}
        </h3>
        <RiskBadge risk={ask.risk} />
        <time
          className="parked__at"
          dateTime={ask.parked_at}
          title={`Parked ${formatTimestamp(ask.parked_at)}; it closes on its own after ${formatTimestamp(ask.expires_at)}.`}
        >
          {formatTimestamp(ask.parked_at)}
        </time>
      </header>

      <p className="parked__summary">{ask.summary}</p>
      <p className="parked__reason">
        {who} Aegis would have asked because {ask.reason}.
      </p>

      <DiffPreview detail={ask.detail} />

      <div className="parked__actions">
        <button
          type="button"
          className="button button--danger"
          disabled={busy}
          title={ANSWERS.deny}
          onClick={() => onAnswer(ask.id, "deny")}
        >
          Deny
        </button>
        <button
          type="button"
          className="button"
          disabled={busy}
          title={ANSWERS.allow_once}
          onClick={() => onAnswer(ask.id, "allow_once")}
        >
          Allow once
        </button>
        {standing ? (
          <button
            type="button"
            className="button button--primary"
            disabled={busy}
            title={`This would ${ask.scope_label}`}
            onClick={() => onAnswer(ask.id, "allow_session")}
          >
            {ask.routine_id.length > 0 ? "Allow standing" : "Allow for this session"}
          </button>
        ) : null}
      </div>

      <p className="parked__scope">
        {standing
          ? `“Allow once” covers this one call as written. The other would ${ask.scope_label}`
          : "This one cannot be signed in advance — it is put to a person every time it is asked."}{" "}
        Either way the run picks up where it stopped.
      </p>
    </section>
  );
}
