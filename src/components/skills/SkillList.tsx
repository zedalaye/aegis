/**
 * The runbooks this install can run (PLAN 7.3, Phase 13).
 *
 * A section of the settings panel, beside Identities, because the two answer
 * halves of one question: a skill is what an identity may run, and an identity
 * is who may run a skill. It sits here rather than in the rail even though half
 * the rows come from the open project's folder — the library half is a fact
 * about the application, and splitting the list across two surfaces would make
 * "which `inbox.triage` is going to run" a question you had to look in two
 * places to answer.
 *
 * Each row says the four things that decide whether you would grant it: what it
 * is for, where it came from, what it will touch, and whether it works. There
 * is no editor and no "new skill" button. A runbook is a `SKILL.md` in a folder
 * you own; writing one is what your editor is for, and the row carries the path
 * so you can go and open it.
 *
 * A runbook that will not parse is listed with the reason rather than hidden.
 * The author is the only person who can fix it, and a skill that quietly
 * vanished from the list would tell them nothing at all.
 *
 * Proposals (PLAN 7.13) are listed under the catalog, and there is no Apply
 * button on them either. Applying is a session's `fs_write` of the proposal to
 * `SKILL.md`, and the approval dialog of that write is where the runbook is
 * signed — a button here would be a second write around the gate.
 */

import type { Skill, SkillProposal } from "../../ipc/bindings";
import { useProjects } from "../../state/projects";
import { useSkills } from "../../state/skills";

/** Where a runbook came from, in the words the row can afford. */
const SCOPE: Record<Skill["scope"], string> = {
  library: "library",
  workspace: "this workspace",
};

/** One runbook. */
function Row({ skill }: { readonly skill: Skill }) {
  const broken = skill.problem !== null;

  return (
    <li className={`skill${broken ? " skill--broken" : ""}`}>
      <div className="skill__head">
        <code className="skill__name">{skill.name}</code>
        <span className="skill__scope">{SCOPE[skill.scope]}</span>
        {skill.version.length === 0 ? null : (
          <span className="skill__version">v{skill.version}</span>
        )}
        {skill.shadows ? (
          <span
            className="skill__badge"
            title="A runbook of this name is also in your library. The workspace one is the one that runs — it is the more specific of the two."
          >
            shadows the library
          </span>
        ) : null}
      </div>

      {broken ? (
        <p className="skill__problem" role="status">
          {skill.problem} Until that is fixed, nothing is offered this runbook.
        </p>
      ) : (
        <>
          <p className="skill__summary">{skill.summary}</p>
          {skill.tools.length === 0 ? null : (
            <p className="skill__tools">
              Calls{" "}
              {skill.tools.map((tool) => (
                <code key={tool}>{tool}</code>
              ))}
            </p>
          )}
        </>
      )}

      <p className="skill__path" title={skill.path}>
        {skill.path}
      </p>
    </li>
  );
}

/** Where a proposal stands, in the words the row can afford. */
const PROPOSAL_STATE: Record<SkillProposal["state"], string> = {
  pending: "not applied",
  applied: "applied",
  occupied: "a runbook is already there",
};

/** One proposal. */
function ProposalRow({ proposal }: { readonly proposal: SkillProposal }) {
  const broken = proposal.problem !== null;
  const blocked = broken || proposal.state === "occupied";

  return (
    <li className={`skill${blocked ? " skill--broken" : ""}`}>
      <div className="skill__head">
        <code className="skill__name">{proposal.name}</code>
        <span className="skill__scope">proposed</span>
        {proposal.version.length === 0 ? null : (
          <span className="skill__version">v{proposal.version}</span>
        )}
        <span className="skill__badge">{PROPOSAL_STATE[proposal.state]}</span>
      </div>

      {broken ? (
        <p className="skill__problem" role="status">
          {proposal.problem} A proposal that will not parse is never applied.
        </p>
      ) : (
        <>
          <p className="skill__summary">{proposal.summary}</p>
          {proposal.tools.length === 0 ? null : (
            <p className="skill__tools">
              Would call{" "}
              {proposal.tools.map((tool) => (
                <code key={tool}>{tool}</code>
              ))}
            </p>
          )}
        </>
      )}

      {proposal.state === "occupied" ? (
        <p className="skill__problem" role="status">
          A different <code>SKILL.md</code> is already beside it. Applying never
          replaces a runbook, so this one stays a proposal.
        </p>
      ) : null}

      <p className="skill__path" title={proposal.path}>
        {proposal.path}
      </p>
    </li>
  );
}

export default function SkillList() {
  const project = useProjects((s) => s.detail?.project ?? null);
  const skills = useSkills((s) => s.skills);
  const proposals = useSkills((s) => s.proposals);
  const status = useSkills((s) => s.status);
  const loadFor = useSkills((s) => s.loadFor);

  if (status === "loading" && skills.length === 0) {
    return <p className="settings__note">Reading the runbooks…</p>;
  }

  return (
    <>
      <p className="settings__note">
        A skill is a runbook: a <code>SKILL.md</code> saying when to use it,
        which tools it will call, the steps, how to check the result, and what
        to do when the source it needs is missing. An identity loads one when it
        is about to follow it, and the steps last for that reply only — so a
        library of twenty procedures costs twenty lines of context, not twenty
        procedures. Running one grants nothing: every step is an ordinary tool
        call, put to you exactly as it would be otherwise.
      </p>

      {skills.length === 0 ? (
        <p className="settings__note">
          No runbooks yet. They live in the <code>skills/</code> folder beside
          your projects file, and in <code>skills/</code> inside a workspace —
          one directory per skill, each holding a <code>SKILL.md</code>. Set up
          the shared files in a project and you get <code>inbox.triage</code> to
          copy from.
        </p>
      ) : (
        <ul className="skill__list">
          {skills.map((skill) => (
            <Row key={`${skill.scope}:${skill.name}`} skill={skill} />
          ))}
        </ul>
      )}

      {project === null || proposals.length === 0 ? null : (
        <>
          <p className="settings__note">
            Proposed in <code>{project.name}</code>. A proposal is never run and
            never offered for granting. To apply one, ask a session to copy its{" "}
            <code>PROPOSAL.md</code> to <code>SKILL.md</code> beside it: that
            write is put to you every time, even with writes allowed for the
            session, and reading it there is when you sign the runbook. Granting
            it to an identity is still a separate tick above.
          </p>
          <ul className="skill__list">
            {proposals.map((proposal) => (
              <ProposalRow key={proposal.name} proposal={proposal} />
            ))}
          </ul>
        </>
      )}

      <div className="skill__actions">
        <button
          type="button"
          className="button"
          // The list is measured, never remembered: this is here because a
          // runbook is a file you edit outside Aegis, and pressing it after
          // fixing one is the whole workflow.
          onClick={() => void loadFor(project?.id ?? null)}
        >
          Re-read
        </button>
        {project === null ? (
          <span className="skill__note">
            No project is open, so only your library is listed. A workspace's own
            runbooks appear when you open it.
          </span>
        ) : (
          <span className="skill__note">
            Listing your library and <code>{project.name}</code>. Grant a skill
            to an identity above; nothing runs one it was not granted.
          </span>
        )}
      </div>
    </>
  );
}
