/**
 * Creating or editing one connector (PLAN 7.3, Phase 18).
 *
 * Five fields, and four of them are shaped by what the runtime will accept: an
 * id that is a namespace rather than a label, a program rather than a command
 * line, its arguments one per line, and the names of the environment variables
 * it needs.
 *
 * Two of those are worth explaining on the form itself, because they are where
 * a person's habits from every other MCP host point the wrong way.
 *
 * **Arguments, not a command line.** There is no shell here — the program is
 * spawned with this vector — so `npx -y @scope/pkg /some/dir` is four
 * arguments, not one string. That is the same rule `shell_exec` follows, and
 * for the same reason: an argument vector is a thing a person can read, and a
 * quoted line is a thing they have to parse.
 *
 * **Variable names, not values.** Aegis reads the value out of its own
 * environment when it starts the connector. Nothing typed here is ever written
 * to disk, and there is nowhere in this form to put a token.
 */

import { useState } from "react";

import type { ConnectorDraft, ConnectorView } from "../../ipc/bindings";
import { blankDraft, draftOf, useConnectors } from "../../state/connectors";

/** The refusal that belongs under `field`, if the last save produced one. */
function useFieldError(field: string): string | null {
  const fieldError = useConnectors((s) => s.fieldError);
  return fieldError?.field === field ? fieldError.message : null;
}

/** One labelled input with the refusal that belongs to it. */
function Field({
  id,
  label,
  hint,
  field,
  children,
}: {
  readonly id: string;
  readonly label: string;
  readonly hint?: string;
  readonly field: string;
  readonly children: (props: {
    readonly id: string;
    readonly invalid: boolean;
    readonly describedBy: string | undefined;
  }) => React.ReactNode;
}) {
  const error = useFieldError(field);

  return (
    <div className="field">
      <label className="field__label" htmlFor={id}>
        {label}
      </label>
      {children({
        id,
        invalid: error !== null,
        describedBy: error === null ? undefined : `${id}-error`,
      })}
      {error === null ? (
        hint === undefined ? null : <p className="field__hint">{hint}</p>
      ) : (
        <p className="field__error" id={`${id}-error`} role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

/** A list of strings as one line each. Blanks are dropped. */
function parseLines(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
}

export default function ConnectorForm({
  editing,
}: {
  /** The connector being edited, or `null` while creating one. */
  readonly editing: ConnectorView | null;
}) {
  const busy = useConnectors((s) => s.busy);
  const save = useConnectors((s) => s.save);
  const cancel = useConnectors((s) => s.cancelEdit);

  const [draft, setDraft] = useState<ConnectorDraft>(() =>
    editing === null ? blankDraft() : draftOf(editing),
  );
  // Kept as text rather than derived from the arrays on every keystroke, so a
  // half-typed line is still a line the person can finish.
  const [argsText, setArgsText] = useState(() => draft.args.join("\n"));
  const [envText, setEnvText] = useState(() => draft.env.join("\n"));

  const patch = (change: Partial<ConnectorDraft>) =>
    setDraft((current) => ({ ...current, ...change }));

  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    void save({
      ...draft,
      args: parseLines(argsText),
      env: parseLines(envText),
    });
  };

  return (
    <form className="connectorform" onSubmit={submit}>
      <Field
        id="connector-name"
        label="Name"
        field="name"
        hint="What you will recognize it by. It never reaches a tool name."
      >
        {({ id, invalid, describedBy }) => (
          <input
            id={id}
            className={`field__input${invalid ? " field__input--bad" : ""}`}
            value={draft.name}
            onChange={(event) => patch({ name: event.target.value })}
            aria-invalid={invalid}
            aria-describedby={describedBy}
          />
        )}
      </Field>

      <Field
        id="connector-id"
        label="Id"
        field="id"
        hint="Lower-case letters, digits and hyphens. It is the part before the __ in every tool this connector offers — git becomes git__status — so changing it renames all of them, and identities that were granted the old names no longer hold the new ones."
      >
        {({ id, invalid, describedBy }) => (
          <input
            id={id}
            className={`field__input field__input--mono${invalid ? " field__input--bad" : ""}`}
            value={draft.id}
            onChange={(event) => patch({ id: event.target.value })}
            aria-invalid={invalid}
            aria-describedby={describedBy}
          />
        )}
      </Field>

      <Field
        id="connector-command"
        label="Program"
        field="command"
        hint="The program that speaks MCP on its stdin and stdout — npx, uvx, docker, or the path to a binary. Not a command line: there is no shell here."
      >
        {({ id, invalid, describedBy }) => (
          <input
            id={id}
            className={`field__input field__input--mono${invalid ? " field__input--bad" : ""}`}
            value={draft.command}
            onChange={(event) => patch({ command: event.target.value })}
            placeholder="npx"
            aria-invalid={invalid}
            aria-describedby={describedBy}
          />
        )}
      </Field>

      <Field
        id="connector-args"
        label="Arguments"
        field="args"
        hint="One per line, in order. They are passed to the program directly, so quotes, pipes and $VARIABLES are not expanded by anything."
      >
        {({ id, invalid, describedBy }) => (
          <textarea
            id={id}
            className={`field__input field__input--area field__input--mono${invalid ? " field__input--bad" : ""}`}
            rows={4}
            value={argsText}
            onChange={(event) => setArgsText(event.target.value)}
            placeholder={"-y\n@modelcontextprotocol/server-filesystem\nC:\\Users\\you\\notes"}
            aria-invalid={invalid}
            aria-describedby={describedBy}
          />
        )}
      </Field>

      <Field
        id="connector-env"
        label="Environment"
        field="env"
        hint="The names of the variables this server needs — GITHUB_TOKEN — one per line. Aegis reads their values from its own environment when it starts the connector and never writes them anywhere. The child gets these and the platform minimum, and nothing else this process happens to hold."
      >
        {({ id, invalid, describedBy }) => (
          <textarea
            id={id}
            className={`field__input field__input--area field__input--mono${invalid ? " field__input--bad" : ""}`}
            rows={2}
            value={envText}
            onChange={(event) => setEnvText(event.target.value)}
            placeholder="GITHUB_TOKEN"
            aria-invalid={invalid}
            aria-describedby={describedBy}
          />
        )}
      </Field>

      <label className="connectorform__enabled">
        <input
          type="checkbox"
          checked={draft.enabled}
          onChange={(event) => patch({ enabled: event.target.checked })}
        />
        Start this connector, now and at every launch
      </label>

      <p className="field__hint">
        Saving this starts the program. Its tools then exist — but nobody holds
        them yet: tick them on an identity under Identities, the same way you
        would grant it <code>shell_exec</code>. The built-in Assistant is the
        exception; it holds every tool this build has, connectors included.
      </p>

      <div className="connectorform__actions">
        <button type="submit" className="button button--primary" disabled={busy}>
          {editing === null ? "Add connector" : "Save"}
        </button>
        <button
          type="button"
          className="button"
          onClick={cancel}
          disabled={busy}
        >
          Cancel
        </button>
      </div>
    </form>
  );
}
