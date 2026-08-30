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
 */

import type { Skill } from "../../ipc/bindings";
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

export default function SkillList() {
  const project = useProjects((s) => s.detail?.project ?? null);
  const skills = useSkills((s) => s.skills);
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
