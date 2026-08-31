/**
 * The `allow_session` grants the open session holds, with a way to take each
 * one back (PLAN 3.1, "revocable").
 *
 * A grant is the only thing in Aegis that makes a future tool call run without
 * asking, so it has to be visible while it is in force — not buried behind a
 * settings page the user has no reason to open. It lives beside the transcript
 * of the session that created it, and it disappears when there are none, which
 * is the normal state.
 *
 * Each row is labelled with the runtime's own `scope_label`: the same sentence
 * the approval dialog showed before the grant was created. A list that
 * described a grant differently from the prompt that created it would be a list
 * the user cannot check their memory against.
 *
 * Settings will show this too from Phase 8. It is here now because the grant
 * belongs to the session, and Phase 6 is where a user can first create one.
 */

import type { Grant } from "../../ipc/bindings";
import { useApprovals } from "../../state/approvals";

/**
 * What a grant covers, in words.
 *
 * Mirrors `Grant::scope_label` in `policy/grants.rs`. Duplicated rather than
 * sent over the wire because a grant is a tagged union, not a string — the
 * runtime's own copy is what the *dialog* shows, and this is the list. If the
 * two ever disagree the runtime is right; that is why the wording here is kept
 * deliberately identical.
 */
function scopeLabel(grant: Grant): string {
  switch (grant.kind) {
    case "fs_read_large":
      return "Read any file over 1 MB inside this workspace";
    case "fs_write":
      return "Write any file inside this workspace, except under .git/";
    case "shell":
      return `Run \`${grant.program}\` in this workspace, with any arguments`;
    case "screen_capture":
      return "Capture the primary display";
    case "memory_write":
      return "Remember things as this identity";
    case "handoff_delegate":
      return "Hand briefs to other identities";
  }
}

/** A stable key for a grant. The variant, plus what narrows it. */
function grantKey(grant: Grant): string {
  return grant.kind === "shell" ? `shell:${grant.program}` : grant.kind;
}

export default function GrantList() {
  const grants = useApprovals((s) => s.grants);
  const revoke = useApprovals((s) => s.revoke);

  if (grants.length === 0) {
    return null;
  }

  return (
    <section className="grants" aria-label="Allowed for this session">
      <h2 className="grants__title">Allowed for this session</h2>
      <ul className="grants__list">
        {grants.map((grant) => (
          <li key={grantKey(grant)} className="grant">
            <span className="grant__scope">{scopeLabel(grant)}</span>
            <button
              type="button"
              className="grant__revoke"
              onClick={() => void revoke(grant)}
            >
              Revoke
            </button>
          </li>
        ))}
      </ul>
      <p className="grants__note">
        These last until this session closes or Aegis quits — never longer.
        Revoking one means you are asked again next time.
      </p>
    </section>
  );
}
