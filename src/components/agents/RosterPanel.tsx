/**
 * A project's roster proposal, and the apply that creates it (PLAN 7.14).
 *
 * Under Identities, because what it creates is identities. A session running
 * `cabinet.found` writes `.aegis/roster/PROPOSAL.md`; this panel shows what
 * applying it would do, identity by identity, and applying it is the grant.
 *
 * The preview *is* the allow-lists. Every tool and runbook a new identity would
 * hold is spelled out, the way a row in the list above is, because "a Chief of
 * Staff" is not an answer to "may this write to my repo". A name that already
 * exists is marked as skipped and its proposed lists are not shown: apply never
 * widens it, and drawing a wider list beside it would read as if it might.
 *
 * What the file says about clocks and connectors is listed as it was written
 * and acted on nowhere here. A routine is saved in Routines once its identity
 * has run the runbook under watch; a connector is installed in Connectors.
 */

import { useEffect } from "react";

import type { RosterEntry } from "../../ipc/bindings";
import { useAgents } from "../../state/agents";
import { useProjects } from "../../state/projects";
import { useRoster } from "../../state/roster";

/** What apply would do with an entry, in the words the row can afford. */
const STATE: Record<RosterEntry["state"], string> = {
  new: "would be created",
  present: "already exists — skipped",
  builtin: "the built-in Assistant — never changed",
};

/** One proposed identity. */
function Entry({ entry }: { readonly entry: RosterEntry }) {
  const fresh = entry.state === "new";
  const broken = entry.problem !== null;

  return (
    <li
      className={`agent roster__entry${broken ? " roster__entry--broken" : ""}${fresh ? "" : " roster__entry--skipped"}`}
    >
      <div className="agent__head">
        <span className="agent__name">{entry.name}</span>
        <span className="agent__badge">{STATE[entry.state]}</span>
      </div>

      {entry.role.length === 0 ? null : (
        <p className="agent__role">{entry.role}</p>
      )}

      {fresh ? (
        <>
          {entry.tools.length === 0 ? (
            <p className="agent__tools agent__tools--none">
              No tools. It could read the conversation and answer; it could not
              touch the machine.
            </p>
          ) : (
            <p className="agent__tools">
              {entry.tools.map((tool) => (
                <code key={tool}>{tool}</code>
              ))}
            </p>
          )}
          {entry.skills.length === 0 ? null : (
            <p className="agent__skills">
              May run{" "}
              {entry.skills.map((skill) => (
                <code key={skill}>{skill}</code>
              ))}
            </p>
          )}
          <p className="agent__role">
            {entry.runs_per_day === 0
              ? "No scheduled runs: nothing may put it on a clock."
              : `Up to ${entry.runs_per_day} scheduled runs a day, once a routine exists.`}
          </p>
        </>
      ) : (
        <p className="agent__role">
          Apply leaves it exactly as it is. A roster never widens an identity
          that exists; edit it above if it should hold more.
        </p>
      )}

      {broken ? (
        <p className="skill__problem" role="status">
          {entry.problem} Until that is fixed, nothing in this roster is
          created.
        </p>
      ) : null}
      {entry.notes.map((note) => (
        <p key={note} className="roster__note">
          {note}
        </p>
      ))}
    </li>
  );
}

/** A bulleted list from the file, or nothing. */
function Listed({
  title,
  items,
  hint,
}: {
  readonly title: string;
  readonly items: readonly string[];
  readonly hint: string;
}) {
  if (items.length === 0) {
    return null;
  }
  return (
    <div className="roster__listed">
      <p className="roster__title">{title}</p>
      <ul className="roster__items">
        {items.map((item) => (
          <li key={item}>{item}</li>
        ))}
      </ul>
      <p className="roster__note">{hint}</p>
    </div>
  );
}

export default function RosterPanel() {
  const project = useProjects((s) => s.detail?.project ?? null);
  const agents = useAgents((s) => s.agents);
  const proposal = useRoster((s) => s.proposal);
  const busy = useRoster((s) => s.busy);
  const confirming = useRoster((s) => s.confirming);
  const applied = useRoster((s) => s.applied);
  const error = useRoster((s) => s.error);
  const loadFor = useRoster((s) => s.loadFor);
  const startConfirm = useRoster((s) => s.startConfirm);
  const cancelConfirm = useRoster((s) => s.cancelConfirm);
  const apply = useRoster((s) => s.apply);

  const projectId = project?.id ?? null;

  // Re-read when the panel opens, when the project changes, and whenever the
  // identity list does: which names already exist is half of the preview.
  useEffect(() => {
    void loadFor(projectId);
  }, [projectId, agents, loadFor]);

  if (project === null || proposal === null) {
    return null;
  }

  const creates = proposal.entries.filter((entry) => entry.state === "new");

  return (
    <section className="roster" aria-label="Roster proposal">
      <p className="settings__note">
        A roster is proposed for <code>{project.name}</code>. Nobody in it
        exists yet. Applying creates the identities marked{" "}
        <em>would be created</em> with exactly the tools and runbooks shown —
        that press is the grant. It writes no routine, starts no connector and
        lays down no world, and names already on file are skipped.
      </p>

      {proposal.problem === null ? (
        <ul className="agent__list">
          {proposal.entries.map((entry) => (
            <Entry key={entry.name} entry={entry} />
          ))}
        </ul>
      ) : (
        <p className="skill__problem" role="status">
          {proposal.problem} A roster that will not parse is never applied.
        </p>
      )}

      <Listed
        title="Intended routines"
        items={proposal.intended_routines}
        hint="Intended, not created. A routine is saved under Routines, once its identity has run that runbook while you watched."
      />
      <Listed
        title="Open questions"
        items={proposal.open_questions}
        hint="What the founder could not settle. A connector named here is installed under Connectors, by you; apply grants no tool nothing answers to."
      />

      {applied === null ? null : (
        <p className="roster__note" role="status">
          {applied.created.length === 0
            ? "Nothing was created."
            : `Created ${applied.created.map((agent) => agent.name).join(", ")}.`}
          {applied.skipped.length === 0
            ? ""
            : ` Skipped ${applied.skipped.join(", ")}, which already existed.`}{" "}
          Each is on the audit log as <code>agent_create</code>, by you.
        </p>
      )}

      {error === null ? null : (
        <p className="skill__problem" role="alert">
          {error.message}
        </p>
      )}

      {confirming ? (
        <div className="roster__confirm">
          <p className="roster__note">
            Create {creates.map((entry) => entry.name).join(", ")} with the
            tools and runbooks listed above? They are in force on the next turn
            of any session opened as them.
          </p>
          <div className="skill__actions">
            <button
              type="button"
              className="button button--primary"
              onClick={() => void apply()}
              disabled={busy}
            >
              {busy ? "Creating…" : "Create them"}
            </button>
            <button
              type="button"
              className="button"
              onClick={cancelConfirm}
              disabled={busy}
            >
              Cancel
            </button>
          </div>
        </div>
      ) : (
        <div className="skill__actions">
          <button
            type="button"
            className="button"
            onClick={startConfirm}
            disabled={!proposal.appliable || busy}
            title={
              proposal.appliable
                ? undefined
                : "Nothing to create: the roster will not parse, one of its identities would be refused, or every name already exists."
            }
          >
            Apply roster…
          </button>
          <button
            type="button"
            className="button"
            onClick={() => void loadFor(projectId)}
            disabled={busy}
          >
            Re-read
          </button>
        </div>
      )}

      <p className="skill__path" title={proposal.path}>
        {proposal.path}
      </p>
    </section>
  );
}
