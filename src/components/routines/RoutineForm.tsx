/**
 * Creating or editing one routine (PLAN 7.3, Phase 16).
 *
 * Identity, runbook and standing approvals are picked from valid options; name,
 * schedule and budget are typed. The runtime enforces the door. No message
 * field.
 */

import { useEffect, useState } from "react";

import type {
  Agent,
  Grant,
  Routine,
  RoutineDraft,
  Schedule,
  Skill,
} from "../../ipc/bindings";
import { useAgents } from "../../state/agents";
import { useProjects } from "../../state/projects";
import { useRoutines, blankDraft, draftOf } from "../../state/routines";
import { useSkills } from "../../state/skills";
import { useSpend } from "../../state/spend";
import { grantKey, sameGrant, toolOf } from "../../lib/grants";

import SpendCapsFields from "../spend/SpendCapsFields";

import { grantLabel } from "./RoutineList";

/**
 * Every fixed standing approval a routine could carry, in the order they are
 * shown. `fs_write` over the whole workspace is not here: it is offered after
 * the runbook's own write prefixes, as a widening (PLAN 7.23).
 */
const SIGNABLE: readonly Grant[] = [
  { kind: "fs_read_large" },
  { kind: "screen_capture" },
  { kind: "memory_write" },
  { kind: "handoff_delegate" },
  // PLAN 7.18: model-written questions to TypeSafe may be signed in advance.
  { kind: "jev_ask" },
];

/** Minutes in a day, for what a cadence asks of a routine. */
const DAY_MINUTES = 1440;

/**
 * How many runs a cadence asks for in a day, or `null` when it asks for no
 * fixed number — a watched folder fires as often as it changes, at most once
 * per five minutes.
 */
function impliedRuns(schedule: Schedule): number | null {
  switch (schedule.kind) {
    case "every":
      return Math.floor(DAY_MINUTES / schedule.minutes);
    case "daily_at":
      return 1;
    default:
      return null;
  }
}

/**
 * What the clock and the two daily ceilings add up to, in one sentence.
 *
 * Three numbers bound a routine and nothing on this form said how they meet:
 * how often it may fire, how many times it may fire, and how many times the
 * identity may be fired at across every routine that runs as it. The tightest
 * wins, and a cadence that asks for more than it will get should say so here
 * rather than be discovered a day later in the ledger.
 */
function ceilingsSentence(
  schedule: Schedule,
  routineCap: number,
  identity: Agent | undefined,
): string {
  const identityCap = identity?.runs_per_day ?? 0;
  const held = Math.min(routineCap, identityCap);
  const asks = impliedRuns(schedule);

  if (identityCap === 0) {
    return `${identity?.name ?? "This identity"} is allowed no scheduled runs a day, so this routine would never fire.`;
  }
  if (asks === null) {
    return `A change is acted on at most every five minutes, and stops after ${held} runs in a day — ${routineCap} is this routine's ceiling, ${identityCap} is ${identity?.name ?? "the identity"}'s across every routine that fires as it.`;
  }
  if (asks <= held) {
    return `That is ${asks} run${asks === 1 ? "" : "s"} a day, inside this routine's ${routineCap} and ${identity?.name ?? "the identity"}'s ${identityCap}.`;
  }
  const binding =
    identityCap <= routineCap
      ? `${identity?.name ?? "the identity"}'s ${identityCap} across every routine that fires as it`
      : `this routine's ${routineCap}`;
  return `That is ${asks} runs a day, more than ${binding}: it will stop after ${held} and start again tomorrow.`;
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

  // Whether both cap fields hold an amount (PLAN 7.26); saving is refused
  // until they do, rather than sending the last amount that parsed.
  const [capsReadable, setCapsReadable] = useState(true);
  const spendError = useFieldError("spend");
  const spentToday = useSpend((s) =>
    editing === null ? undefined : (s.today.routines[editing.id] ?? 0),
  );
  const refreshSpend = useSpend((s) => s.refresh);
  useEffect(() => {
    void refreshSpend();
  }, [refreshSpend]);

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
  const writes = (chosen?.tools.includes("fs_write") ?? false) &&
    (identity?.tools.includes("fs_write") ?? false);

  // Where the runbook says its writes land, first; the whole workspace only as
  // a widening, and said so (PLAN 7.23).
  const narrow: Grant[] = writes
    ? (chosen?.writes ?? []).map((prefix) => ({ kind: "fs_write_under", prefix }))
    : [];
  const widening: Grant | null = writes ? { kind: "fs_write" } : null;

  const signable: Grant[] = SIGNABLE.filter((grant) => {
    const tool = toolOf(grant);
    return (
      (chosen?.tools.includes(tool) ?? false) &&
      (identity?.tools.includes(tool) ?? false)
    );
  });

  // Connector tools are not a fixed list — they are named by servers somebody
  // installed — so the signable ones are derived from the same two halves as
  // the rest: declared by the runbook, held by the identity. One approval per
  // tool, because that is what the grant covers.
  for (const tool of chosen?.tools ?? []) {
    if (
      tool.includes("__") &&
      (identity?.tools.includes(tool) ?? false)
    ) {
      signable.push({ kind: "connector", tool });
    }
  }

  // What is already signed and offered by nothing above — a folder or a
  // command shape added by answering a parked ask *allow standing* — stays
  // on the list, so unticking it is as visible as ticking it was.
  const offered = [...narrow, ...(widening ? [widening] : []), ...signable];
  const held = draft.grants.filter(
    (grant) => !offered.some((one) => sameGrant(one, grant)),
  );
  const rows: { grant: Grant; widens: boolean }[] = [
    ...narrow.map((grant) => ({ grant, widens: false })),
    ...held.map((grant) => ({ grant, widens: false })),
    ...(widening ? [{ grant: widening, widens: narrow.length > 0 }] : []),
    ...signable.map((grant) => ({ grant, widens: false })),
  ];

  const toggleGrant = (grant: Grant, on: boolean) =>
    patch({
      grants: on
        ? [...draft.grants, grant]
        : draft.grants.filter((one) => !sameGrant(one, grant)),
    });

  const schedule = draft.schedule;
  const grantsError = useFieldError("grants");

  return (
    <form
      className="routineform"
      onSubmit={(event) => {
        event.preventDefault();
        if (capsReadable) {
          void save(draft);
        }
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
          for a week owes one run. A folder is looked at, not watched: saving
          this records where it stands, and anything that touches it afterwards
          — a file added, moved in, rewritten, renamed or deleted — is a change,
          at most as often as the five-minute floor allows.
        </p>
      </fieldset>

      <fieldset className="routineform__grants">
        <legend className="field__label">Standing approvals</legend>
        {draft.skill === "" ? (
          <p className="field__hint">Pick a runbook first.</p>
        ) : rows.length === 0 ? (
          <p className="field__hint">
            This runbook declares nothing that would ask. Its runs will read and
            report, which is the safest kind of routine there is.
          </p>
        ) : (
          <ul className="routineform__grantlist">
            {rows.map(({ grant, widens }) => (
              <li key={grantKey(grant)}>
                <label className="routineform__grant">
                  <input
                    type="checkbox"
                    checked={draft.grants.some((one) => sameGrant(one, grant))}
                    onChange={(event) => toggleGrant(grant, event.target.checked)}
                  />
                  <span>
                    {widens ? "Widen: " : ""}
                    {grantLabel(grant)}
                  </span>
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

      <p className="field__hint">
        {ceilingsSentence(draft.schedule, draft.runs_per_day, identity)}
      </p>

      <SpendCapsFields
        idPrefix="routine-spend"
        value={draft.spend}
        onChange={(next) => {
          setCapsReadable(next !== null);
          if (next !== null) {
            patch({ spend: next });
          }
        }}
        error={spendError}
        runMeans="one fire of this routine, however many turns it takes — a resumed run included"
        spentToday={spentToday}
      />

      <div className="routineform__actions">
        <button
          type="submit"
          className="button button--primary"
          disabled={busy || !capsReadable}
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
