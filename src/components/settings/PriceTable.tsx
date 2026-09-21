/**
 * What each model costs on one provider row (PLAN 7.26).
 *
 * The operator's word, per million tokens, in dollars. Read by the model-spend
 * caps: a capped identity or routine whose model has no price here is refused
 * rather than let through unmeasured. Saved on its own, apart from the row.
 *
 * *Suggest prices* fills the table from the row's catalog or LiteLLM's table,
 * and saves nothing: a figure read wrong would move every cap that uses it.
 */

import { useState } from "react";

import type {
  MaskedProvider,
  ModelPrice,
  PriceSuggestion,
} from "../../ipc/bindings";
import { settingsSuggestPrices } from "../../ipc/commands";
import { toIpcError } from "../../lib/errors";
import { editableDollars, parseDollars } from "../../lib/money";
import { useAgents } from "../../state/agents";
import { useSettings } from "../../state/settings";

const SOURCE_LABELS = {
  catalog: "this provider's catalog",
  litellm: "LiteLLM's public price table",
} as const;

/** What the last suggestion did, in sentences. */
function suggestionNotes(suggestion: PriceSuggestion): string[] {
  const notes: string[] = [];
  for (const source of ["catalog", "litellm"] as const) {
    const found = suggestion.prices.filter((one) => one.source === source);
    if (found.length > 0) {
      const names = found
        .map((one) =>
          one.matched === null
            ? one.price.model
            : `${one.price.model} (as ${one.matched})`,
        )
        .join(", ");
      notes.push(`From ${SOURCE_LABELS[source]}: ${names}.`);
    }
  }
  if (suggestion.missing.length > 0) {
    notes.push(
      `Not found anywhere: ${suggestion.missing.join(", ")}. Type those yourself.`,
    );
  }
  notes.push(...suggestion.notes);
  if (suggestion.prices.length > 0) {
    notes.push("Nothing is saved yet: check the figures, then Save prices.");
  }
  return notes;
}

/** One line as typed. */
type Line = {
  readonly model: string;
  readonly input: string;
  readonly output: string;
  readonly cacheRead: string;
  readonly cacheWrite: string;
};

function lineOf(price: ModelPrice): Line {
  return {
    model: price.model,
    input: editableDollars(price.input),
    output: editableDollars(price.output),
    cacheRead: editableDollars(price.cache_read),
    cacheWrite: editableDollars(price.cache_write),
  };
}

/** A line as a price, or `null` while one of its amounts is unreadable. */
function priceOf(line: Line): ModelPrice | null {
  const input = parseDollars(line.input);
  const output = parseDollars(line.output);
  const read = parseDollars(line.cacheRead);
  const write = parseDollars(line.cacheWrite);
  if (input.kind !== "amount" || output.kind !== "amount") {
    return null;
  }
  if (read.kind === "invalid" || write.kind === "invalid") {
    return null;
  }
  return {
    model: line.model.trim(),
    input: input.micros,
    output: output.micros,
    cache_read: read.kind === "amount" ? read.micros : null,
    cache_write: write.kind === "amount" ? write.micros : null,
  };
}

const COLUMNS: readonly { key: keyof Omit<Line, "model">; label: string }[] = [
  { key: "input", label: "Input" },
  { key: "output", label: "Output" },
  { key: "cacheRead", label: "Cache read" },
  { key: "cacheWrite", label: "Cache write" },
];

export default function PriceTable({ row }: { readonly row: MaskedProvider }) {
  const busy = useSettings((s) => s.busy);
  const savePrices = useSettings((s) => s.savePrices);
  const fieldError = useSettings((s) => s.fieldError);

  const agents = useAgents((s) => s.agents);
  const [lines, setLines] = useState<Line[]>(() => row.prices.map(lineOf));
  const [suggesting, setSuggesting] = useState(false);
  const [notes, setNotes] = useState<string[]>([]);

  // The models this row answers with: its own, the ones already priced, and
  // those of identities bound to it.
  const wanted = [
    row.model,
    ...lines.map((line) => line.model.trim()),
    ...agents
      .filter((agent) => agent.provider_id === row.id)
      .map((agent) => agent.model),
  ].filter((model, at, all) => model.length > 0 && all.indexOf(model) === at);

  const suggest = async () => {
    setSuggesting(true);
    setNotes([]);
    try {
      const suggestion = await settingsSuggestPrices(row.id, wanted);
      setLines((current) => {
        const next = [...current];
        for (const { price } of suggestion.prices) {
          const line = lineOf(price);
          const at = next.findIndex((one) => one.model.trim() === price.model);
          if (at === -1) {
            next.push(line);
          } else {
            next[at] = line;
          }
        }
        return next;
      });
      setNotes(suggestionNotes(suggestion));
    } catch (cause) {
      setNotes([toIpcError(cause, "settings_suggest_prices").message]);
    } finally {
      setSuggesting(false);
    }
  };
  const prices = lines.map(priceOf);
  const readable = prices.every((price) => price !== null);
  const error = fieldError?.field === "prices" ? fieldError.message : null;

  const change = (at: number, patch: Partial<Line>) =>
    setLines((current) =>
      current.map((line, index) => (index === at ? { ...line, ...patch } : line)),
    );

  return (
    <fieldset className="prices">
      <legend className="field__label">Prices, $ per million tokens</legend>

      {lines.length === 0 ? (
        <p className="field__hint">
          No model on this row has a price, so what its turns cost is not
          measured, and an identity or routine with a model budget cannot run
          on it.
        </p>
      ) : (
        <table className="prices__table">
          <thead>
            <tr>
              <th scope="col">Model</th>
              {COLUMNS.map((column) => (
                <th key={column.key} scope="col">
                  {column.label}
                </th>
              ))}
              <th scope="col" aria-label="Remove" />
            </tr>
          </thead>
          <tbody>
            {lines.map((line, at) => (
              <tr key={at}>
                <td>
                  <input
                    className="field__input"
                    aria-label="Model"
                    value={line.model}
                    spellCheck={false}
                    autoComplete="off"
                    onChange={(event) => change(at, { model: event.target.value })}
                    disabled={busy}
                  />
                </td>
                {COLUMNS.map((column) => {
                  const optional =
                    column.key === "cacheRead" || column.key === "cacheWrite";
                  const parsed = parseDollars(line[column.key]);
                  const bad =
                    parsed.kind === "invalid" ||
                    (!optional && parsed.kind === "blank");
                  return (
                    <td key={column.key}>
                      <input
                        className={`field__input field__input--number${bad ? " field__input--bad" : ""}`}
                        aria-label={`${column.label}, $ per million tokens`}
                        inputMode="decimal"
                        placeholder={optional ? "= input" : ""}
                        value={line[column.key]}
                        onChange={(event) =>
                          change(at, { [column.key]: event.target.value })
                        }
                        aria-invalid={bad}
                        disabled={busy}
                      />
                    </td>
                  );
                })}
                <td>
                  <button
                    type="button"
                    className="link"
                    onClick={() =>
                      setLines((current) => current.filter((_, i) => i !== at))
                    }
                    disabled={busy}
                  >
                    Remove
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {error === null ? (
        <p className="field__hint">
          Matched to the model a turn sends, exactly. A blank cache price
          charges the input price, which errs high. A CLI login has a price
          too: what you decide a token is worth, not what an invoice says.
        </p>
      ) : (
        <p className="field__error" role="alert">
          {error}
        </p>
      )}

      {notes.length === 0 ? null : (
        <ul className="prices__notes">
          {notes.map((note) => (
            <li key={note}>{note}</li>
          ))}
        </ul>
      )}

      <div className="prices__actions">
        <button
          type="button"
          className="button"
          onClick={() => void suggest()}
          disabled={busy || suggesting || wanted.length === 0}
          title="Reads this provider's model catalog where it publishes prices, then LiteLLM's public table on GitHub. Fills the table; saves nothing."
        >
          {suggesting ? "Looking up prices…" : "Suggest prices"}
        </button>
        <button
          type="button"
          className="button"
          onClick={() =>
            setLines((current) => [
              ...current,
              {
                model: current.some((line) => line.model === row.model)
                  ? ""
                  : row.model,
                input: "",
                output: "",
                cacheRead: "",
                cacheWrite: "",
              },
            ])
          }
          disabled={busy}
        >
          Add a price
        </button>
        <button
          type="button"
          className="button button--primary"
          onClick={() => {
            const ready = prices.filter(
              (price): price is ModelPrice => price !== null,
            );
            if (readable) {
              void savePrices(ready);
            }
          }}
          disabled={busy || !readable}
        >
          Save prices
        </button>
      </div>
    </fieldset>
  );
}
