/**
 * The `allow_session` grants the open session holds, with a way to take each
 * one back (PLAN 3.1, "revocable").
 *
 * Shown beside the session's transcript while any grant is in force, using the
 * same wording as the approval dialog.
 */

import type { Grant } from "../../ipc/bindings";
import { useApprovals } from "../../state/approvals";

/**
 * What a grant covers, in words. Must match `Grant::scope_label` in
 * `policy/grants.rs` word for word.
 */
function scopeLabel(grant: Grant): string {
  switch (grant.kind) {
    case "fs_read_large":
      return "Read any file over 1 MB inside this workspace";
    case "fs_write":
      return "Write any file inside this workspace, except under .git/ and world/";
    case "world_amend":
      return "Amend world/, this workspace’s constitution";
    case "shell":
      if (grant.program === "git") {
        return "Run read-only `git` in this workspace (status, log, diff, show, …) — any other verb, an option before the verb, and a line that writes a file or runs a program are still asked about";
      }
      return `Run \`${grant.program}\` in this workspace, with any arguments`;
    case "screen_capture":
      return "Capture the primary display";
    case "memory_write":
      return "Remember things as this identity";
    case "handoff_delegate":
      return "Hand briefs to other identities";
    case "connector":
      // The whole tool name, because that is the scope: approving
      // `git__status` approved `git__status`, not the `git` connector and not
      // whatever it offers next week.
      return `Call \`${grant.tool}\` with any arguments`;
  }
}

/** A stable key for a grant. The variant, plus what narrows it. */
function grantKey(grant: Grant): string {
  if (grant.kind === "shell") {
    return `shell:${grant.program}`;
  }
  // Two connector grants differ by the tool they name, so the variant alone
  // would collapse them into one row.
  if (grant.kind === "connector") {
    return `connector:${grant.tool}`;
  }
  return grant.kind;
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
