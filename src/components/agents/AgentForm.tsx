/**
 * Creating or editing one identity (PLAN 7.3, Phases 12 and 13).
 *
 * Four things to fill in and two allow-lists: a name, a role, what it should
 * carry into every request, the tools it holds, and the runbooks it may run.
 * The two lists are the part that matters, so they are sets of checkboxes and
 * chips rather than text fields — an allow-list you type is an allow-list you
 * typo.
 *
 * Neither list is written here. The tools come from the built-in identity,
 * which holds every tool this build has by construction, so it *is* the
 * catalogue; the skills come from the runbooks the runtime found on disk. Both
 * would drift if the UI kept a copy.
 *
 * The skills field stays a text input under those chips, because a workspace
 * runbook is only discoverable while that project is open, and an identity has
 * to be grantable a skill that is not in front of you right now.
 *
 * A refused value lands under the input it is about, like the provider form's,
 * because the runtime says which field it was talking about.
 */

import { useEffect, useState } from "react";

import type { Agent, AgentDraft } from "../../ipc/bindings";
import {
  DEFAULT_AGENT_ID,
  blankDraft,
  draftOf,
  useAgents,
} from "../../state/agents";
import { useSkills } from "../../state/skills";

/** What each tool does, in the fewest words that distinguish it. */
const TOOL_SUMMARY: Record<string, string> = {
  fs_list: "list folders in the workspace",
  fs_read: "read files",
  fs_write: "write files",
  shell_exec: "run programs",
  screen_capture: "capture the screen",
  skill_run: "load a runbook it was granted",
  skill_return: "record what a runbook produced",
  memory_write: "remember something, for every later session",
  memory_search: "look through what it remembers",
  handoff_delegate: "hand briefs to other identities and wait for them",
  handoff_return: "report back on a brief it was handed",
};

/**
 * The two tools an identity granted a skill has to hold.
 *
 * Mirrors the check in `store/agents.rs`: a runbook it cannot load is a grant
 * that does nothing. They are ticked here rather than added silently on save,
 * so the widening is something the user watches happen and can undo — which is
 * the difference between an affordance and an allow-list that grows by itself.
 */
const SKILL_TOOLS = ["skill_run", "skill_return"] as const;

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

/** `text` with `name` added, or removed if it was already there. */
function toggleSkill(text: string, name: string): string {
  const names = parseSkills(text);
  const without = names.filter((granted) => granted !== name);

  return (without.length === names.length ? [...names, name] : without).join(
    ", ",
  );
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
  const known = useSkills((s) => s.skills);

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

  const granted = parseSkills(skillsText);

  // Granting a runbook to an identity that cannot load one is a grant that does
  // nothing, and the runtime refuses to save it. Ticking the two boxes here is
  // that rule made visible *before* the save rather than reported after it —
  // and it only ever adds, so a user who unticks one is not fought with.
  // Keyed on the text rather than on the parsed list: that list is a new array
  // on every render, and an effect keyed on it would never stop running.
  useEffect(() => {
    if (parseSkills(skillsText).length === 0) {
      return;
    }
    setDraft((current) => {
      const missing = SKILL_TOOLS.filter(
        (tool) => !current.tools.includes(tool),
      );
      return missing.length === 0
        ? current
        : { ...current, tools: [...current.tools, ...missing] };
    });
  }, [skillsText]);

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
        hint="Carried into every request this identity makes. Say what it is, not how to carry out a procedure — a procedure is a skill, and a skill is loaded only when it is used."
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
        // A text field under the chips, not only chips: a workspace runbook is
        // discoverable while that project is open and not otherwise, and an
        // identity has to be grantable a skill that is not in front of you.
        hint="The runbooks this identity may load. It is never offered one it was not granted, and holding one never adds a tool — a runbook that calls a tool from the list above and does not have it is refused before its first step."
      >
        {({ id, invalid, describedBy }) => (
          <>
            {known.length === 0 ? null : (
              <ul className="agentform__skillpicks">
                {known.map((skill) => {
                  const on = granted.includes(skill.name);
                  return (
                    <li key={`${skill.scope}:${skill.name}`}>
                      <button
                        type="button"
                        className={`chip${on ? " chip--on" : ""}`}
                        aria-pressed={on}
                        title={
                          skill.problem === null
                            ? `${skill.summary} (${skill.scope === "workspace" ? "this workspace" : "library"})`
                            : `This runbook will not run as written: ${skill.problem}`
                        }
                        onClick={() =>
                          setSkillsText((text) => toggleSkill(text, skill.name))
                        }
                      >
                        {skill.name}
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
            <input
              id={id}
              className={`field__input${invalid ? " field__input--bad" : ""}`}
              value={skillsText}
              onChange={(event) => setSkillsText(event.target.value)}
              spellCheck={false}
              autoComplete="off"
              placeholder="inbox.triage, never-send-without-review"
              aria-invalid={invalid}
              aria-describedby={describedBy}
            />
          </>
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
