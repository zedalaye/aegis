/**
 * The shared-workspace convention, in the rail (PLAN 7.3, Phase 11).
 *
 * Five directories in the open project's folder — `briefs/`, `status/`,
 * `artefacts/`, `decisions/` and, from Phase 13, `skills/` — with one button to
 * lay down whatever is missing. That is the whole surface. The files themselves
 * are read and written by the conversation, through `fs_write` and the approval
 * dialog, which is why there is no editor here: a panel that could rewrite
 * `DECISIONS.md` without passing the gate would be a second write path around
 * it.
 *
 * It sits in the rail rather than the work area because it is a fact about the
 * project, not about the session — and it stays collapsed to a single line once
 * everything is there, since a convention that is already in place is not
 * something to look at every day.
 */

import { useProjects } from "../../state/projects";
import { useSkills } from "../../state/skills";
import { useWorkspace } from "../../state/workspace";

/** One directory, and whether it and its file are there. */
function Entry({
  dir,
  file,
  present,
}: {
  readonly dir: string;
  readonly file: string;
  readonly present: boolean;
}) {
  return (
    <li className={`shared__entry${present ? "" : " shared__entry--missing"}`}>
      <span aria-hidden="true" className="shared__mark">
        {present ? "✓" : "·"}
      </span>
      <span className="shared__dir">{dir}/</span>
      <span className="shared__file" title={file}>
        {file.slice(dir.length + 1)}
      </span>
      <span className="shared__state">{present ? "" : "not there"}</span>
    </li>
  );
}

export default function SharedFiles() {
  const project = useProjects((s) => s.detail?.project ?? null);
  const layout = useWorkspace((s) => s.layout);
  const status = useWorkspace((s) => s.status);
  const busy = useWorkspace((s) => s.busy);
  const created = useWorkspace((s) => s.created);
  const scaffolded = useWorkspace((s) => s.scaffolded);
  const scaffold = useWorkspace((s) => s.scaffold);
  const loadSkills = useSkills((s) => s.loadFor);

  if (project === null || layout === null || status !== "ready") {
    return null;
  }

  // A project whose folder has been unmounted has nothing to set up, and
  // saying "not there" four times over would blame the convention for a
  // missing disk. The workspace badge above already reports the real problem.
  if (!project.workspace_exists) {
    return null;
  }

  return (
    <section className="shared" aria-label="Shared workspace files">
      <h2 className="sidebar__heading">Shared files</h2>

      {layout.complete ? (
        <p className="shared__note">
          Briefs, status, artefacts, decisions and this project's own runbooks
          are in this folder. Ask for a decision to be recorded and it goes in{" "}
          <code>DECISIONS.md</code>; the runbooks in <code>skills/</code> are
          listed under <em>Settings → Skills</em>.
        </p>
      ) : (
        <>
          <ul className="shared__list">
            {layout.entries.map((entry) => (
              <Entry
                key={entry.dir}
                dir={entry.dir}
                file={entry.file}
                present={entry.dir_exists && entry.file_exists}
              />
            ))}
          </ul>
          <button
            type="button"
            className="button button--wide"
            // The skill catalog is re-measured after, because one of the five
            // directories this creates is `skills/` and one of the seed files
            // is a runbook. The panel that lists runbooks is in another
            // surface and would otherwise be stale until someone thought to
            // press *Re-read* — which is a thing nobody thinks to do about a
            // folder they have just been told was created for them.
            onClick={() =>
              void scaffold(project.id).then(() => loadSkills(project.id))
            }
            disabled={busy}
            // Said on the control rather than in a confirmation: the promise
            // the user needs before pressing it is that nothing of theirs is
            // replaced, and a dialog they learn to dismiss does not make it.
            title="Creates only what is missing. Existing files are left exactly as they are."
          >
            {busy ? "Setting up…" : "Set up shared files"}
          </button>
        </>
      )}

      {scaffolded ? (
        <p className="shared__note" role="status">
          {created.length === 0
            ? "Everything was already there; nothing was changed."
            : `Created ${created.join(", ")}.`}
        </p>
      ) : null}
    </section>
  );
}
