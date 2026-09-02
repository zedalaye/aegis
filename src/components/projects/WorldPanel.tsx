/**
 * The world, in the rail (`PLAN.md` § 7.2 — the missed half of Phase 11).
 *
 * The other layer in the same folder as *Shared files*, with the opposite
 * mutation rule. The cabinet is in-flight work and is rewritten every turn; the
 * constitution is what the project *is*, and specialists read it and do not
 * write it.
 *
 * Two things this panel deliberately does not have.
 *
 * There is **no button**. `Set up shared files` lays down the cabinet because a
 * convention is worth having in an empty folder; a constitution is not. Five
 * empty templates in a workspace with no essence are theatre, and a workspace
 * that has an essence has somebody who can write it. So a world starts in an
 * editor, or as an `fs_write` through the approval dialog, and this panel
 * reports what is there.
 *
 * There is **no editor**, for the reason *Shared files* has none: a panel that
 * could rewrite `essence.md` without passing the gate would be a second write
 * path around it — and this is the one file where that matters most, since
 * amending the world is a human decision.
 *
 * What it is for is the one question the folder cannot answer at a glance:
 * whether the sources this world was perceived from are still the ones on disk.
 * A source that has moved holds every brief that is not about the delta, so
 * seeing it here is seeing why nothing is going out.
 */

import type { SourceState, WorldSource } from "../../ipc/bindings";
import { useProjects } from "../../state/projects";
import { useWorkspace } from "../../state/workspace";

/** How each state of a declared source is named to a person. */
const STATE_LABEL: Record<SourceState, string> = {
  in_step: "",
  drifted: "moved",
  missing: "not there",
  unrecorded: "not recorded",
};

/** One file of the constitution, and whether it is there. */
function Leaf({
  file,
  what,
  exists,
}: {
  readonly file: string;
  readonly what: string;
  readonly exists: boolean;
}) {
  const name = file.slice(file.indexOf("/") + 1);
  return (
    <li className={`shared__entry${exists ? "" : " shared__entry--missing"}`}>
      <span aria-hidden="true" className="shared__mark">
        {exists ? "✓" : "·"}
      </span>
      <span className="shared__file" title={what}>
        {name}
      </span>
      <span className="shared__state">{exists ? "" : "not written"}</span>
    </li>
  );
}

/** One declared source, and what is true of it right now. */
function Source({ source }: { readonly source: WorldSource }) {
  const drifted = source.state !== "in_step";
  return (
    <li className={`shared__entry${drifted ? " world__entry--drifted" : ""}`}>
      <span aria-hidden="true" className="shared__mark">
        {drifted ? "!" : "✓"}
      </span>
      <span className="shared__file" title={source.path}>
        {source.path}
      </span>
      <span className="shared__state">{STATE_LABEL[source.state]}</span>
    </li>
  );
}

export default function WorldPanel() {
  const project = useProjects((s) => s.detail?.project ?? null);
  const world = useWorkspace((s) => s.world);
  const status = useWorkspace((s) => s.status);

  if (project === null || world === null || status !== "ready") {
    return null;
  }
  if (!project.workspace_exists) {
    return null;
  }

  // A folder with no world gets one muted line, not a list of five files it
  // does not have. Saying nothing at all would make the feature reachable only
  // from the README; laying out the templates would be the theatre this panel
  // exists not to do.
  if (!world.present) {
    return (
      <section className="shared" aria-label="World">
        <h2 className="sidebar__heading">World</h2>
        <p className="shared__note">
          No constitution in this folder. A project that has an essence worth
          protecting keeps it in <code>world/</code>, starting with{" "}
          <code>essence.md</code>. Write that file — or ask a session to, with{" "}
          <code>world.draft</code> — and it appears here.
        </p>
      </section>
    );
  }

  return (
    <section className="shared" aria-label="World">
      <h2 className="sidebar__heading">World</h2>

      <ul className="shared__list">
        {world.files.map((file) => (
          <Leaf
            key={file.file}
            file={file.file}
            what={file.what}
            exists={file.exists}
          />
        ))}
      </ul>

      <p className="shared__note">
        Delegated work reads <code>world/</code> and cannot write it. Amending it
        is yours: ask in a session and every write is put to you, at high risk,
        under an approval that reaches <code>world/</code> and nothing else.
      </p>

      {world.sources.length > 0 ? (
        <>
          <ul className="shared__list">
            {world.sources.map((source) => (
              <Source key={source.path} source={source} />
            ))}
          </ul>
          <p
            className="shared__note"
            role={world.drifted ? "status" : undefined}
          >
            {world.drifted
              ? "A source has moved since this world was perceived from it. Until that delta is perceived, briefs that are not about it will not go out."
              : "What this world was perceived from. Sessions cannot reopen these; what they said is in the files above."}
          </p>
        </>
      ) : null}

      {world.problem === null ? null : (
        <p className="shared__note" role="status">
          {world.problem}
        </p>
      )}
    </section>
  );
}
