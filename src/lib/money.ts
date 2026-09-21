/**
 * Money as the spend caps hold it (PLAN 7.26): whole micro-dollars on the
 * wire, dollars on screen. Nothing here decides anything; the runtime checks
 * every amount again.
 */

/** One dollar, in micro-dollars. Mirrors `DOLLAR` in `store/ledger.rs`. */
export const DOLLAR = 1_000_000;

/**
 * Micro-dollars as `$0.50`: two decimals, or up to six when the amount needs
 * them, so a price of $0.075 per million reads as itself.
 */
export function formatDollars(micros: number): string {
  const whole = Math.floor(micros / DOLLAR);
  const frac = micros % DOLLAR;
  if (frac % 10_000 === 0) {
    return `$${whole}.${String(frac / 10_000).padStart(2, "0")}`;
  }
  return `$${whole}.${String(frac).padStart(6, "0").replace(/0+$/, "")}`;
}

/** An amount as a person typed it: `$0.50`, `0.5`, `12`. */
export type Parsed =
  | { readonly kind: "blank" }
  | { readonly kind: "amount"; readonly micros: number }
  | { readonly kind: "invalid" };

/**
 * Parses dollars into micro-dollars, exactly: at most six decimals, no float
 * arithmetic on the fraction.
 */
export function parseDollars(text: string): Parsed {
  const trimmed = text.trim().replace(/^\$/, "").trim();
  if (trimmed.length === 0) {
    return { kind: "blank" };
  }
  const match = /^(\d{1,7})(?:\.(\d{0,6}))?$/.exec(trimmed);
  if (match === null) {
    return { kind: "invalid" };
  }
  const whole = Number(match[1]);
  const frac = Number((match[2] ?? "").padEnd(6, "0"));
  return { kind: "amount", micros: whole * DOLLAR + frac };
}

/** Micro-dollars as an editable field: no `$`, no trailing zeros past cents. */
export function editableDollars(micros: number | null): string {
  return micros === null ? "" : formatDollars(micros).slice(1);
}
