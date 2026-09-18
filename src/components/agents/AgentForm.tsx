/**
 * Creating or editing one identity (PLAN 7.3, Phases 12 and 13).
 *
 * Allow-lists are checkboxes and chips, not free text. Built-in tools come from
 * the built-in identity (it holds all of them), skills from the runtime's
 * catalog, and connector tools (Phase 18) from live connectors, granted by full
 * name; stale granted names are listed, not dropped. Skills also accept typed
 * names, for workspace runbooks of other projects. Refusals land under their
 * field.
 */

import { useEffect, useState } from "react";

import type { Agent, AgentDraft } from "../../ipc/bindings";
import {
  DEFAULT_AGENT_ID,
  blankDraft,
  draftOf,
  useAgents,
} from "../../state/agents";
import { liveTools, useConnectors } from "../../state/connectors";
import {
  DEFAULT_PROVIDER_ID,
  providerName,
  rowOf,
  useSettings,
} from "../../state/settings";
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
  jev_eval: "run a signed project eval (questions the cabinet already holds)",
  jev_ask: "draft typed questions the harness does not yet own",
};

/**
 * The tools a skill grant requires (checked in `store/agents.rs`), ticked
 * visibly rather than added on save.
 */
const SKILL_TOOLS = ["skill_run", "skill_return"] as const;

/**
 * Connector tools held and offered (Phase 18), in their own fieldset; granted
 * names no longer offered are still listed.
 */
function ConnectorTools({
  draft,
  toggle,
}: {
  readonly draft: AgentDraft;
  readonly toggle: (tool: string, granted: boolean) => void;
}) {
  const connectors = useConnectors((s) => s.connectors);
  const live = liveTools(connectors);

  const stranded = draft.tools.filter(
    (name) =>
      name.includes("__") && !live.some((tool) => tool.full_name === name),
  );

  if (live.length === 0 && stranded.length === 0) {
    return null;
  }

  return (
    <fieldset className="agentform__tools">
      <legend className="field__label">Connector tools</legend>
      <ul className="agentform__toollist">
        {live.map((tool) => (
          <li key={tool.full_name}>
            <label className="agentform__tool">
              <input
                type="checkbox"
                checked={draft.tools.includes(tool.full_name)}
                onChange={(event) =>
                  toggle(tool.full_name, event.target.checked)
                }
              />
              <code>{tool.full_name}</code>
              <span className="agentform__toolnote">{tool.description}</span>
            </label>
          </li>
        ))}
      </ul>
      {stranded.length === 0 ? null : (
        <p className="field__hint">
          Granted but not offered right now:{" "}
          {stranded.map((name) => (
            <code key={name}>{name}</code>
          ))}
          . The connector is stopped or gone; the grant is kept, and a call to
          it is refused until it comes back.
        </p>
      )}
      <p className="field__hint">
        Granted one at a time, by full name — holding <code>git__status</code>{" "}
        does not hold anything else the <code>git</code> connector offers, and
        does not hold a tool it adds tomorrow. Ticking one never skips the
        dialog: every connector call is put to you before it runs.
      </p>
    </fieldset>
  );
}

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
  const settings = useSettings((s) => s.settings);
  // What a *new* identity opens filled in with: a duplicate's fields, or
  // nothing. Read once, with the draft below, for the same reason.
  const seed = useAgents((s) => s.seed);

  // Seeded once, from whatever the form was opened on. Re-seeding on every
  // render would throw away what the user is typing; the store closes the form
  // on a successful save, which unmounts this and takes the draft with it.
  const [draft, setDraft] = useState<AgentDraft>(() =>
    editing === null ? (seed ?? blankDraft()) : draftOf(editing),
  );

  // Held as the text that was typed, parsed only on submit. Parsing on every
  // keystroke and rendering the result back would eat the comma the moment it
  // was typed, because a trailing empty name is dropped.
  const [skillsText, setSkillsText] = useState(() =>
    (editing?.skills ?? seed?.skills ?? []).join(", "),
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

  // Tick the skill tools when skills are typed (only ever adds). Keyed on the
  // text: the parsed list is a new array every render.
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

      <Field
        id="agent-provider"
        label="Provider"
        field="provider"
        hint="Which provider answers for this identity. A session opened as it can switch its own from the chat header; that never changes the identity."
      >
        {({ id, invalid, describedBy }) => (
          <select
            id={id}
            className={`field__input${invalid ? " field__input--bad" : ""}`}
            value={draft.provider_id}
            onChange={(event) => patch({ provider_id: event.target.value })}
            aria-invalid={invalid}
            aria-describedby={describedBy}
          >
            {(settings?.providers ?? []).map((row) => (
              <option key={row.id} value={row.id}>
                {providerName(row)}
              </option>
            ))}
            {/* A binding to a row that is gone stays visible until changed. */}
            {settings === null ||
            rowOf(settings, draft.provider_id) !== undefined ? null : (
              <option value={draft.provider_id}>
                Not on file ({draft.provider_id})
              </option>
            )}
          </select>
        )}
      </Field>

      <Field
        id="agent-model"
        label="Model"
        field="model"
        hint="Leave empty to use the provider's own model, and follow it when that changes."
      >
        {({ id, invalid, describedBy }) => (
          <input
            id={id}
            className={`field__input${invalid ? " field__input--bad" : ""}`}
            value={draft.model}
            onChange={(event) => patch({ model: event.target.value })}
            placeholder={
              rowOf(settings, draft.provider_id)?.model ||
              rowOf(settings, DEFAULT_PROVIDER_ID)?.model ||
              "the provider's model"
            }
            spellCheck={false}
            autoComplete="off"
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

      <ConnectorTools draft={draft} toggle={toggleTool} />

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

      <Field
        id="agent-runs"
        label="Scheduled runs a day"
        field="runs_per_day"
        // The ceiling on the *role*, said where the role is edited. Three
        // well-behaved routines on one identity can still spend a night
        // writing, and this is the number that catches that (PLAN 7.3,
        // Phase 16).
        hint="A ceiling on what a clock may start as this identity, across every routine that fires as it. Each routine has a budget of its own as well. Sessions you type into are not counted — you are the budget."
      >
        {({ id, invalid, describedBy }) => (
          <input
            id={id}
            type="number"
            className={`field__input field__input--number${invalid ? " field__input--bad" : ""}`}
            min={0}
            max={200}
            value={draft.runs_per_day}
            onChange={(event) =>
              patch({ runs_per_day: Number(event.target.value) })
            }
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
