/**
 * The shared-workspace convention, in the rail (PLAN 7.3, Phase 11).
 *
 * The `.aegis/` directories and whether the folder is versioned, with one
 * button to lay down what is missing (no editor; browsing is *Files*,
 * PLAN 7.15). `world/` is separate ({@link WorldPanel}). Collapses to one line
 * once the directories and the git work tree (PLAN 7.11) are both present.
 */

import type { Versioning } from "../../ipc/bindings";
import Section from "../layout/Section";
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
  // The `.aegis/` every row shares is dropped from the label and kept in the
  // tooltip: it is on the heading already, and five copies of it down a narrow
  // rail crowds out the half that differs.
  const name = dir.slice(dir.indexOf("/") + 1);
  return (
    <li className={`shared__entry${present ? "" : " shared__entry--missing"}`}>
      <span aria-hidden="true" className="shared__mark">
        {present ? "✓" : "·"}
      </span>
      <span className="shared__dir" title={dir}>
        {name}/
      </span>
      <span className="shared__file" title={file}>
        {file.slice(dir.length + 1)}
      </span>
      <span className="shared__state">{present ? "" : "not there"}</span>
    </li>
  );
}

/**
 * Whether the folder has a history (PLAN 7.11). One sentence, no git client:
 * committing happens in a terminal or through the session's gate.
 */
function Versioned({ versioning }: { readonly versioning: Versioning }) {
  if (versioning.tree === "here") {
    return (
      <p className="shared__note">
        A git repository, so these files have a history. Nothing here commits —
        ask in the session and <code>git</code> goes through the approval
        dialog.
      </p>
    );
  }

  if (versioning.tree === "ancestor") {
    return (
      <p className="shared__note">
        {/*
          "a second repository", not "a second one": this line sits beside a
          control whose whole job is to create something inside that repo, and
          the promise being made is only about `.git`.
        */}
        Versioned by <code>{versioning.at}</code>, above this folder. Nothing
        here will make a second repository inside it.
      </p>
    );
  }

  // One sentence for both controls below — the wide button when the files are
  // missing too, the link when only this is. What each one does is on its own
  // label; what they both promise is here.
  return (
    <p className="shared__note">
      Not a git repository, so these files have no history. Making one never
      commits and never adds a remote.
    </p>
  );
}

export default function SharedFiles() {
  const project = useProjects((s) => s.detail?.project ?? null);
  const layout = useWorkspace((s) => s.layout);
  const status = useWorkspace((s) => s.status);
  const busy = useWorkspace((s) => s.busy);
  const created = useWorkspace((s) => s.created);
  const initialized = useWorkspace((s) => s.initialized);
  const problem = useWorkspace((s) => s.problem);
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

  // The convention is laid down *and* the folder has a history. Both halves,
  // because a workspace scaffolded before PLAN 7.11 existed has every tick and
  // no repository — and a panel that called that done would strand it with
  // nothing to press (`complete` alone used to, and did).
  const versioned = layout.versioning.tree !== "unversioned";
  const settled = layout.complete && versioned;

  // One door for both halves: `workspace_scaffold` creates what is missing and
  // versions the folder, so a folder needing only the second half asks for the
  // same command. A second command would be a second thing to keep in step.
  const press = () =>
    void scaffold(project.id).then(() => loadSkills(project.id));

  return (
    <Section
      id="shared"
      title="Shared files"
      className="shared"
      badge={
        settled ? null : (
          <span className="rail__count rail__count--attention">set up</span>
        )
      }
    >
      {layout.complete ? (
        <p className="shared__note">
          Briefs, status, artefacts, decisions and this project's runbooks are
          in <code>.aegis/</code>. Ask for a decision to be recorded and it goes
          in <code>DECISIONS.md</code>; the runbooks are under{" "}
          <em>Settings → Skills</em>.
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
            // Scaffolding seeds a runbook, so re-measure the skill catalog.
            onClick={press}
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

      <Versioned versioning={layout.versioning} />

      {/*
        A link rather than the wide button, because the files are already
        there: what is left is one small missing thing, not a set-up step. It
        calls the same command — scaffolding is idempotent, and the report then
        says everything was already there and what it did about the repository.
      */}
      {layout.complete && !versioned ? (
        <button type="button" className="link" onClick={press} disabled={busy}>
          {busy ? "Working…" : "Make it a git repository"}
        </button>
      ) : null}

      {scaffolded ? (
        <p className="shared__note" role="status">
          {created.length === 0
            ? "Everything was already there."
            : `Created ${created.join(", ")}.`}{" "}
          {/*
            Said in the same breath as what was created, because it is the same
            answer to the same question — what did pressing that do to my
            folder. "It", not "this folder is a git repository": the line above
            has just said that, and the half worth reading twice is the promise
            at the end. A run that found a repository already there says
            neither.
          */}
          {initialized ? "Made it a repository; nothing was committed." : null}
          {problem === null ? null : `Not versioned: ${problem}.`}
        </p>
      ) : null}

      {/*
        These five used to sit at the root of the folder. Naming them is all
        this does — moving somebody's `DECISIONS.md` for them is exactly the
        act the button next to this one refuses to do, and a folder full of
        work is the worst possible thing to be clever with.
      */}
      {layout.strays.length > 0 ? (
        <p className="shared__note" role="status">
          {layout.strays.map((dir) => `${dir}/`).join(", ")}{" "}
          {layout.strays.length === 1 ? "is" : "are"} at the top of this folder,
          from the earlier layout. Move{" "}
          {layout.strays.length === 1 ? "it" : "them"} into <code>.aegis/</code>{" "}
          and the contents are found again; nothing here will move{" "}
          {layout.strays.length === 1 ? "it" : "them"} for you.
        </p>
      ) : null}
    </Section>
  );
}
