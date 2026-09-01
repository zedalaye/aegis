/**
 * Creating or editing one routine (PLAN 7.3, Phase 16).
 *
 * Five things to decide, and the form is shaped so that four of them can only
 * be answered with something the runtime would accept: the identity comes from
 * the registry, the runbook from that identity's own allow-list, and the
 * standing approvals from the tools that runbook says it will call. What is
 * left to type is a name, an interval and a budget.
 *
 * That is deliberate. The door this form stands in front of — a live skill,
 * already granted, already run under watch — is enforced in Rust and is the
 * whole point of the phase, so the form's job is to make the *shape* of a
 * routine obvious and let the runtime say the one thing it alone knows: whether
 * anybody has actually watched this runbook run.
 *
 * There is no message field, and there is nowhere to put one. A routine names a
 * runbook; a routine that could carry a paragraph would be a chat on a timer.
 */

import { useState } from "react";

import type { Agent, Grant, Routine, RoutineDraft, Skill } from "../../ipc/bindings";
import { useAgents } from "../../state/agents";
import { useProjects } from "../../state/projects";
import { useRoutines, blankDraft, draftOf } from "../../state/routines";
import { useSkills } from "../../state/skills";

import { grantLabel } from "./RoutineList";

/** Every standing approval a routine could carry, in the order they are shown. */
const SIGNABLE: readonly Grant[] = [
  { kind: "fs_write" },
  { kind: "fs_read_large" },
  { kind: "screen_capture" },
  { kind: "memory_write" },
  { kind: "handoff_delegate" },
];

/** The tool a grant can ever apply to. Mirrors `Grant::tool` in Rust. */
function toolOf(grant: Grant): string {
  switch (grant.kind) {
    case "fs_read_large":
      return "fs_read";
    case "fs_write":
      return "fs_write";
    case "shell":
      return "shell_exec";
    case "screen_capture":
      return "screen_capture";
    case "memory_write":
      return "memory_write";
    case "handoff_delegate":
      return "handoff_delegate";
  }
}

/** Whether two grants are the same standing approval. */
function same(one: Grant, other: Grant): boolean {
  return one.kind === other.kind &&
    (one.kind !== "shell" || one.program === (other as { program: string }).program);
}

/** The refusal that belongs under `field`, if the last save produced one. */
function useFieldError(field: string): string | null {
  const fieldError = useRoutines((s) => s.fieldError);
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

export default function RoutineForm({
  editing,
}: {
  /** The routine being edited, or `null` while creating one. */
  readonly editing: Routine | null;
}) {
  const busy = useRoutines((s) => s.busy);
  const save = useRoutines((s) => s.save);
  const cancel = useRoutines((s) => s.cancelEdit);
  const agents = useAgents((s) => s.agents);
  const skills = useSkills((s) => s.skills);
  const projects = useProjects((s) => s.projects);
  const openProject = useProjects((s) => s.detail?.project ?? null);

  // Identities that could ever fire a routine: the built-in one holds every
  // tool and no skills by construction, so it can never name a runbook.
  const identities = agents.filter((agent) => !agent.builtin);

  const [draft, setDraft] = useState<RoutineDraft>(() =>
    editing === null
      ? blankDraft(
          openProject?.id ?? projects[0]?.id ?? "",
          identities[0]?.id ?? "",
        )
      : draftOf(editing),
  );

  const patch = (change: Partial<RoutineDraft>) =>
    setDraft((current) => ({ ...current, ...change }));

  const identity: Agent | undefined = identities.find(
    (agent) => agent.id === draft.agent_id,
  );

  // The runbooks this identity was granted, which is exactly the set the door
  // would accept. A name granted but not on disk is still listed — the runtime
  // refuses it with the reason, which is more useful than a name that silently
  // never appears.
  const grantable = identity?.skills ?? [];
  const chosen: Skill | undefined = skills.find(
    (skill) => skill.name === draft.skill,
  );

  // What this routine could be signed for: declared by the runbook, held by the
  // identity. Both halves are checked again in Rust; showing only what would
  // pass is what keeps the form from offering an approval that cannot be saved.
  const signable = SIGNABLE.filter((grant) => {
    const tool = toolOf(grant);
    return (
      (chosen?.tools.includes(tool) ?? false) &&
      (identity?.tools.includes(tool) ?? false)
    );
  });

  const toggleGrant = (grant: Grant, on: boolean) =>
    patch({
      grants: on
        ? [...draft.grants, grant]
        : draft.grants.filter((held) => !same(held, grant)),
    });

  const schedule = draft.schedule;
  const grantsError = useFieldError("grants");

  return (
    <form
      className="routineform"
      onSubmit={(event) => {
        event.preventDefault();
        void save(draft);
      }}
    >
      <Field
        id="routine-name"
        label="Name"
        field="name"
        hint="What the list shows. Morning watch, Inbox sweep."
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
        id="routine-project"
        label="Project"
        field="project"
        hint="Its runs happen in this project's workspace, and nowhere else."
      >
        {({ id, invalid, describedBy }) => (
          <select
            id={id}
            className={`field__input${invalid ? " field__input--bad" : ""}`}
            value={draft.project_id}
            onChange={(event) => patch({ project_id: event.target.value })}
            aria-invalid={invalid}
            aria-describedby={describedBy}
          >
            {projects.map((project) => (
              <option key={project.id} value={project.id}>
                {project.name}
              </option>
            ))}
          </select>
        )}
      </Field>

      <Field
        id="routine-agent"
        label="Identity"
        field="identity"
        hint="It runs as this identity, under that identity's allow-list — the same one a session gets."
      >
        {({ id, invalid, describedBy }) => (
          <select
            id={id}
            className={`field__input${invalid ? " field__input--bad" : ""}`}
            value={draft.agent_id}
            onChange={(event) =>
              // The runbook and the approvals both belong to the identity that
              // was chosen, so changing it clears them rather than carrying a
              // grant across to somebody who was never given the tool.
              patch({
                agent_id: event.target.value,
                skill: "",
                grants: [],
              })
            }
            aria-invalid={invalid}
            aria-describedby={describedBy}
          >
            {identities.length === 0 ? (
              <option value="">No identity to run as yet</option>
            ) : null}
            {identities.map((agent) => (
              <option key={agent.id} value={agent.id}>
                {agent.name}
              </option>
            ))}
          </select>
        )}
      </Field>

      <Field
        id="routine-skill"
        label="Runbook"
        field="skill"
        hint="Only what this identity was granted, and only once you have watched it run at least once. That check reads the audit log; there is no way round it."
      >
        {({ id, invalid, describedBy }) => (
          <select
            id={id}
            className={`field__input${invalid ? " field__input--bad" : ""}`}
            value={draft.skill}
            onChange={(event) =>
              patch({ skill: event.target.value, grants: [] })
            }
            aria-invalid={invalid}
            aria-describedby={describedBy}
          >
            <option value="">Pick a runbook</option>
            {grantable.map((name) => (
              <option key={name} value={name}>
                {name}
              </option>
            ))}
          </select>
        )}
      </Field>

      <fieldset className="routineform__schedule">
        <legend className="field__label">When</legend>

        <label className="routineform__choice">
          <input
            type="radio"
            name="routine-schedule"
            checked={schedule.kind === "every"}
            onChange={() => patch({ schedule: { kind: "every", minutes: 60 } })}
          />
          Every
          {schedule.kind === "every" ? (
            <>
              <input
                type="number"
                className="field__input field__input--number"
                min={5}
                max={1440}
                value={schedule.minutes}
                onChange={(event) =>
                  patch({
                    schedule: {
                      kind: "every",
                      minutes: Number(event.target.value),
                    },
                  })
                }
              />
              minutes, counted from the last run
            </>
          ) : (
            <span className="routineform__aside">so many minutes</span>
          )}
        </label>

        <label className="routineform__choice">
          <input
            type="radio"
            name="routine-schedule"
            checked={schedule.kind === "daily_at"}
            onChange={() =>
              patch({ schedule: { kind: "daily_at", hour: 7, minute: 0 } })
            }
          />
          Daily at
          {schedule.kind === "daily_at" ? (
            <input
              type="time"
              className="field__input field__input--number"
              value={`${String(schedule.hour).padStart(2, "0")}:${String(
                schedule.minute,
              ).padStart(2, "0")}`}
              onChange={(event) => {
                const [hour, minute] = event.target.value.split(":");
                patch({
                  schedule: {
                    kind: "daily_at",
                    hour: Number(hour ?? 0),
                    minute: Number(minute ?? 0),
                  },
                });
              }}
            />
          ) : (
            <span className="routineform__aside">a time of day, local</span>
          )}
        </label>

        <label className="routineform__choice">
          <input
            type="radio"
            name="routine-schedule"
            checked={schedule.kind === "on_change"}
            onChange={() =>
              patch({ schedule: { kind: "on_change", dir: "briefs" } })
            }
          />
          When a folder changes:
          {schedule.kind === "on_change" ? (
            <input
              className="field__input field__input--number"
              value={schedule.dir}
              spellCheck={false}
              autoComplete="off"
              onChange={(event) =>
                patch({
                  schedule: { kind: "on_change", dir: event.target.value },
                })
              }
            />
          ) : (
            <span className="routineform__aside">
              a directory inside the workspace
            </span>
          )}
        </label>

        <p className="field__hint">
          A missed window fires once, never a backlog: a machine that was asleep
          for a week owes one run. A folder is looked at, not watched — the
          first look records what is there, and only something newer than that
          fires it, at most as often as the five-minute floor allows.
        </p>
      </fieldset>

      <fieldset className="routineform__grants">
        <legend className="field__label">Standing approvals</legend>
        {draft.skill === "" ? (
          <p className="field__hint">Pick a runbook first.</p>
        ) : signable.length === 0 ? (
          <p className="field__hint">
            This runbook declares nothing that would ask. Its runs will read and
            report, which is the safest kind of routine there is.
          </p>
        ) : (
          <ul className="routineform__grantlist">
            {signable.map((grant) => (
              <li key={grant.kind}>
                <label className="routineform__grant">
                  <input
                    type="checkbox"
                    checked={draft.grants.some((held) => same(held, grant))}
                    onChange={(event) => toggleGrant(grant, event.target.checked)}
                  />
                  <span>{grantLabel(grant)}</span>
                </label>
              </li>
            ))}
          </ul>
        )}
        {grantsError === null ? (
          <p className="field__hint">
            Nobody can answer a dialog while this runs, so what you tick here is
            the whole of what it may do beyond reading — signed once, now, in
            the same words the approval dialog would have used. Everything else
            is refused outright and reported as blocked. Nothing outside the
            workspace, and nothing under <code>.git/</code>, can be signed for
            at all.
          </p>
        ) : (
          <p className="field__error" role="alert">
            {grantsError}
          </p>
        )}
      </fieldset>

      <Field
        id="routine-budget"
        label="Runs a day"
        field="budget"
        hint="A ceiling nobody has to be awake to enforce. The identity has one of its own, across every routine that fires as it."
      >
        {({ id, invalid, describedBy }) => (
          <input
            id={id}
            type="number"
            className={`field__input field__input--number${invalid ? " field__input--bad" : ""}`}
            min={1}
            max={96}
            value={draft.runs_per_day}
            onChange={(event) =>
              patch({ runs_per_day: Number(event.target.value) })
            }
            aria-invalid={invalid}
            aria-describedby={describedBy}
          />
        )}
      </Field>

      <div className="routineform__actions">
        <button
          type="submit"
          className="button button--primary"
          disabled={busy}
        >
          {busy ? "Saving…" : editing === null ? "Create routine" : "Save"}
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
