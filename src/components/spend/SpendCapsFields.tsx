/**
 * The two model-spend caps of a routine or an identity (PLAN 7.26).
 *
 * Typed in dollars, held as the text typed, and handed up as micro-dollars
 * only when both fields parse — an amount the form cannot read is reported as
 * `null`, so the parent can refuse to save rather than send the last good one.
 */

import { useState } from "react";

import type { SpendCaps } from "../../ipc/bindings";
import { editableDollars, formatDollars, parseDollars } from "../../lib/money";

/** One field's text, and what it means. */
function read(text: string): number | null | undefined {
  const parsed = parseDollars(text);
  switch (parsed.kind) {
    case "blank":
      return null;
    case "amount":
      return parsed.micros;
    default:
      return undefined;
  }
}

export default function SpendCapsFields({
  idPrefix,
  value,
  onChange,
  error,
  runMeans,
  spentToday,
}: {
  /** Prefix for the two inputs' ids. */
  readonly idPrefix: string;
  /** The caps as last saved or edited. */
  readonly value: SpendCaps;
  /** The caps typed, or `null` while either field is not an amount. */
  readonly onChange: (next: SpendCaps | null) => void;
  /** The runtime's refusal for `spend`, if the last save produced one. */
  readonly error: string | null;
  /** What one run is, for this owner, in a few words. */
  readonly runMeans: string;
  /** What the owner has spent today; `undefined` for one not saved yet. */
  readonly spentToday: number | undefined;
}) {
  const [runText, setRunText] = useState(() => editableDollars(value.per_run));
  const [dayText, setDayText] = useState(() => editableDollars(value.per_day));

  const run = read(runText);
  const day = read(dayText);

  const update = (nextRun: string, nextDay: string) => {
    const perRun = read(nextRun);
    const perDay = read(nextDay);
    onChange(
      perRun === undefined || perDay === undefined
        ? null
        : { per_run: perRun, per_day: perDay },
    );
  };

  const unreadable = run === undefined || day === undefined;

  return (
    <fieldset className="spendcaps">
      <legend className="field__label">Model budget</legend>

      <label className="spendcaps__row" htmlFor={`${idPrefix}-per-run`}>
        <span>Per run $</span>
        <input
          id={`${idPrefix}-per-run`}
          className={`field__input field__input--number${run === undefined ? " field__input--bad" : ""}`}
          inputMode="decimal"
          placeholder="no cap"
          value={runText}
          onChange={(event) => {
            setRunText(event.target.value);
            update(event.target.value, dayText);
          }}
          aria-invalid={run === undefined}
        />
      </label>

      <label className="spendcaps__row" htmlFor={`${idPrefix}-per-day`}>
        <span>Per day $</span>
        <input
          id={`${idPrefix}-per-day`}
          className={`field__input field__input--number${day === undefined ? " field__input--bad" : ""}`}
          inputMode="decimal"
          placeholder="no cap"
          value={dayText}
          onChange={(event) => {
            setDayText(event.target.value);
            update(runText, event.target.value);
          }}
          aria-invalid={day === undefined}
        />
        {spentToday === undefined ? null : (
          <span className="spendcaps__spent">
            {formatDollars(spentToday)} spent today
          </span>
        )}
      </label>

      {error !== null ? (
        <p className="field__error" role="alert">
          {error}
        </p>
      ) : unreadable ? (
        <p className="field__error" role="alert">
          An amount in dollars, like 0.50 — or blank for no cap.
        </p>
      ) : (
        <p className="field__hint">
          A run is {runMeans}. A day is UTC. Past a cap, the tools a turn
          asked for are refused and the model gets one reply to wrap up; a
          turn that starts past it sends nothing. A cap needs the model's
          price under Settings → Providers — without one, its turns are
          refused rather than let through unmeasured.
        </p>
      )}
    </fieldset>
  );
}
