/**
 * Creating or editing one identity (PLAN 7.3, Phase 12).
 *
 * Four things to fill in and one to tick: a name, a role, what it should carry
 * into every request, and which tools it holds. The tool list is the part that
 * matters, so it is a set of checkboxes rather than a text field — an
 * allow-list you type is an allow-list you typo.
 *
 * The tools are read from the identities the runtime already sent rather than
 * from a list written here. The built-in identity holds every tool this build
 * has, by construction, so it *is* the catalogue — and one that cannot drift
 * out of step with the registry the way a copy in the UI would.
 *
 * A refused value lands under the input it is about, like the provider form's,
 * because the runtime says which field it was talking about.
 */

import { useState } from "react";

import type { Agent, AgentDraft } from "../../ipc/bindings";
import {
  DEFAULT_AGENT_ID,
  blankDraft,
  draftOf,
  useAgents,
} from "../../state/agents";

/** What each tool does, in the fewest words that distinguish it. */
const TOOL_SUMMARY: Record<string, string> = {
  fs_list: "list folders in the workspace",
  fs_read: "read files",
  fs_write: "write files",
  shell_exec: "run programs",
  screen_capture: "capture the screen",
};

/** The tools this build has, taken from the identity that holds them all. */
function catalogue(agents: readonly Agent[]): readonly string[] {
  return agents.find((agent) => agent.id === DEFAULT_AGENT_ID)?.tools ?? [];
}

/** The skill names in a comma-separated field. Blanks are dropped. */
function parseSkills(text: string): string[] {
  return text
    .split(",")
    .map((name) => name.trim())
    .filter((name) => name.length > 0);
}

/** The refusal that belongs under `field`, if the last save produced one. */
function useFieldError(field: string): string | null {
  const fieldError = useAgents((s) => s.fieldError);
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

export default function AgentForm({
  editing,
}: {
  /** The identity being edited, or `null` while creating one. */
  readonly editing: Agent | null;
}) {
  const agents = useAgents((s) => s.agents);
  const busy = useAgents((s) => s.busy);
  const save = useAgents((s) => s.save);
  const cancel = useAgents((s) => s.cancelEdit);
  const toolsError = useFieldError("tools");

  // Seeded once, from whatever the form was opened on. Re-seeding on every
  // render would throw away what the user is typing; the store closes the form
  // on a successful save, which unmounts this and takes the draft with it.
  const [draft, setDraft] = useState<AgentDraft>(() =>
    editing === null ? blankDraft() : draftOf(editing),
  );

  // Held as the text that was typed, parsed only on submit. Parsing on every
  // keystroke and rendering the result back would eat the comma the moment it
  // was typed, because a trailing empty name is dropped.
  const [skillsText, setSkillsText] = useState(() =>
    (editing?.skills ?? []).join(", "),
  );

  const patch = (change: Partial<AgentDraft>) =>
    setDraft((current) => ({ ...current, ...change }));

  const toggleTool = (tool: string, granted: boolean) =>
    patch({
      tools: granted
        ? [...draft.tools, tool]
        : draft.tools.filter((name) => name !== tool),
    });

  return (
    <form
      className="agentform"
      onSubmit={(event) => {
        event.preventDefault();
        void save({ ...draft, skills: parseSkills(skillsText) });
      }}
    >
      <Field
        id="agent-name"
        label="Name"
        field="name"
        hint="What the session picker shows. Reviewer, Scribe, Triager."
      >
        {({ id, invalid, describedBy }) => (
          <input
            id={id}
            className={`field__input${invalid ? " field__input--bad" : ""}`}
            value={draft.name}
            onChange={(event) => patch({ name: event.target.value })}
            spellCheck={false}
            autoComplete="off"
            aria-invalid={invalid}
            aria-describedby={describedBy}
          />
        )}
      </Field>

      <Field
        id="agent-role"
        label="Role"
        field="role"
        hint="One line saying what this identity is for."
      >
        {({ id, invalid, describedBy }) => (
          <input
            id={id}
            className={`field__input${invalid ? " field__input--bad" : ""}`}
            value={draft.role}
            onChange={(event) => patch({ role: event.target.value })}
            aria-invalid={invalid}
            aria-describedby={describedBy}
          />
        )}
      </Field>

      <Field
        id="agent-instructions"
        label="Instructions"
        field="instructions"
        // The cap is enforced in Rust, and the reason for it is said here
        // rather than only in the refusal: a form that explains the rule before
        // it is broken is worth more than one that explains it after.
        hint="Carried into every request this identity makes. Say what it is, not how to carry out a procedure — a runbook is a skill, and skills come later."
      >
        {({ id, invalid, describedBy }) => (
          <textarea
            id={id}
            className={`field__input field__input--area${invalid ? " field__input--bad" : ""}`}
            rows={4}
            value={draft.instructions}
            onChange={(event) => patch({ instructions: event.target.value })}
            aria-invalid={invalid}
            aria-describedby={describedBy}
          />
        )}
      </Field>

      <fieldset className="agentform__tools">
        <legend className="field__label">Tools</legend>
        <ul className="agentform__toollist">
          {catalogue(agents).map((tool) => (
            <li key={tool}>
              <label className="agentform__tool">
                <input
                  type="checkbox"
                  checked={draft.tools.includes(tool)}
                  onChange={(event) => toggleTool(tool, event.target.checked)}
                />
                <code>{tool}</code>
                <span className="agentform__toolnote">
                  {TOOL_SUMMARY[tool] ?? ""}
                </span>
              </label>
            </li>
          ))}
        </ul>
        {toolsError === null ? (
          <p className="field__hint">
            Everything left unticked is refused for this identity, whether or
            not the model asks for it — and it is never offered the tool in the
            first place. Ticking one does not skip the approval dialog: a write
            is still put to you before it runs.
          </p>
        ) : (
          <p className="field__error" role="alert">
            {toolsError}
          </p>
        )}
      </fieldset>

      <Field
        id="agent-skills"
        label="Skills"
        field="skills"
        // Recorded, not runnable. Saying so on the field is the honest thing:
        // a list that silently did nothing would be worse than no list, and
        // hiding it would leave the only way to fill it in hand-editing
        // `agents.json`.
        hint="Comma-separated, like inbox.triage. Nothing runs a skill yet — this build records which ones an identity would be allowed to run, and the runner comes with the next phase. A skill never widens the tools above."
      >
        {({ id, invalid, describedBy }) => (
          <input
            id={id}
            className={`field__input${invalid ? " field__input--bad" : ""}`}
            value={skillsText}
            onChange={(event) => setSkillsText(event.target.value)}
            spellCheck={false}
            autoComplete="off"
            aria-invalid={invalid}
            aria-describedby={describedBy}
          />
        )}
      </Field>

      <div className="agentform__actions">
        <button
          type="submit"
          className="button button--primary"
          disabled={busy}
        >
          {busy ? "Saving…" : editing === null ? "Create identity" : "Save"}
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
