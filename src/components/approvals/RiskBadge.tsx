/**
 * The risk badge on an approval.
 *
 * Advisory only, and the wording says so. `risk` never decides anything in the
 * runtime (PLAN 3.3): it changes the colour and the sentence, not whether the
 * call is asked about or what approving it allows. A badge that looked like a
 * verdict would be worse than no badge, because the thing that actually
 * protects the user is reading the path and the arguments below it.
 */

import type { Risk } from "../../ipc/bindings";

/** What each level is called, and what it means, in one clause. */
const LEVELS: Record<Risk, { readonly label: string; readonly title: string }> =
  {
    low: {
      label: "Low",
      title: "Reversible, and inside the workspace.",
    },
    medium: {
      label: "Medium",
      title: "This changes something, or reaches past what you were looking at.",
    },
    high: {
      label: "High",
      title:
        "Outside the workspace, secret-shaped, or arbitrary code. Read it carefully.",
    },
  };

export default function RiskBadge({ risk }: { readonly risk: Risk }) {
  const level = LEVELS[risk];

  return (
    <span className={`risk risk--${risk}`} title={level.title}>
      {level.label} risk
    </span>
  );
}
