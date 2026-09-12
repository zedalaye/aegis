/**
 * The rail: every project, most recent first, and the open project's sessions
 * beneath them.
 *
 * Two lists in one scrolling column. Adding a project or a session is a "+"
 * on that section's heading, not a footer: the footer stole height from the
 * session list, and "Add workspace" named the folder rather than the row it
 * creates. The project list is the thing a user changes rarely and the
 * session list the thing they change constantly, which is why the sessions sit
 * closer to the conversation they belong to.
 *
 * Between them, on the project's side of that line, are three panels that
 * belong to the folder rather than to any one conversation: where its commands
 * run (PLAN 7.12), the shared files `briefs/`, `status/`, `artefacts/` and
 * `decisions/` (PLAN 7.3, Phase 11), and the world. The execution host is first
 * because it is the one to settle before any work starts; the world is last,
 * for the opposite reason and one stronger — it is the slowest-changing thing
 * here.
 *
 * A project row is also a drop target (PLAN 7.15): a file dropped on it is
 * copied into that project's `.aegis/briefs/`, or refused when the project has
 * no cabinet. See `state/intake.ts`.
 *
 * All of them are {@link Section}s, so any can be folded away and stays that
 * way. The whole stack is more than a laptop screen holds at once, and which
 * part matters is a question about what somebody is doing this week — the world
 * while founding one, the sessions the rest of the time.
 */

import { useProjectDrops } from "../../state/intake";
import { useProjects } from "../../state/projects";
import Section from "./Section";
import RailAdd from "./RailAdd";
import ExecHost from "../projects/ExecHost";
import ProjectPicker from "../projects/ProjectPicker";
import SharedFiles from "../projects/SharedFiles";
import WorldPanel from "../projects/WorldPanel";
import SessionList from "../sessions/SessionList";

/** What the last drop onto a project row did, under the list. */
function DropReport() {
  const projects = useProjects((s) => s.projects);
  const importing = useProjectDrops((s) => s.importing);
  const result = useProjectDrops((s) => s.result);
  const refusal = useProjectDrops((s) => s.refusal);
  const dismiss = useProjectDrops((s) => s.dismiss);

  const nameOf = (projectId: string) =>
    projects.find((project) => project.id === projectId)?.name ?? "that project";

  if (importing !== null) {
    return (
      <p className="shared__note" role="status">
        Copying into {nameOf(importing)}&apos;s <code>.aegis/briefs/</code>…
      </p>
    );
  }
  if (result === null && refusal === null) {
    return null;
  }

  // Anything that did not arrive is drawn as a warning, not as a note: a
  // refusal read at a glance as "done" is a brief somebody thinks is filed.
  const refused = refusal !== null || (result?.report.refused.length ?? 0) > 0;

  return (
    <p
      className={`shared__note rail__drop ${refused ? "rail__drop--refused" : "rail__drop--arrived"}`}
      role={refused ? "alert" : "status"}
    >
      {result === null ? null : (
        <>
          {result.report.arrived.length === 0 ? null : (
            <>
              Copied {result.report.arrived.map((arrival) => arrival.from).join(", ")}{" "}
              into {nameOf(result.projectId)}&apos;s <code>.aegis/briefs/</code>.{" "}
            </>
          )}
          {result.report.refused.map((refused) => (
            <span key={refused.name}>
              {refused.name} was not added: {refused.reason}.{" "}
            </span>
          ))}
        </>
      )}
      {refusal === null
        ? null
        : `${refusal.names.join(", ")} not added to ${nameOf(refusal.projectId)}: ${refusal.message}. `}
      <button type="button" className="link" onClick={dismiss}>
        Dismiss
      </button>
    </p>
  );
}

export default function Sidebar() {
  const projects = useProjects((s) => s.projects);
  const detail = useProjects((s) => s.detail);
  const status = useProjects((s) => s.status);
  const busy = useProjects((s) => s.busy);
  const open = useProjects((s) => s.open);
  const remove = useProjects((s) => s.remove);
  const pickWorkspace = useProjects((s) => s.pickWorkspace);
  const pending = useProjects((s) => s.pending);
  const dropHovered = useProjectDrops((s) => s.hovered);

  const openId = detail?.project.id ?? null;

  return (
    <nav className="sidebar" aria-label="Projects and sessions">
      <Section
        id="projects"
        title="Projects"
        pinned={pending !== null}
        badge={
          projects.length === 0 ? null : (
            <span className="rail__count">{projects.length}</span>
          )
        }
        action={
          <RailAdd
            label="Add project"
            disabled={busy}
            onClick={() => void pickWorkspace()}
          />
        }
      >
        {status === "loading" && projects.length === 0 ? (
          <p className="sidebar__empty">Loading…</p>
        ) : null}

        {status !== "loading" && projects.length === 0 ? (
          <p className="sidebar__empty">
            No projects yet. Add a project to start.
          </p>
        ) : null}

        <ul className="sidebar__list">
          {projects.map((project) => {
            const isOpen = project.id === openId;
            return (
              <li
                key={project.id}
                className="sidebar__item"
                data-drop-project={project.id}
              >
                <button
                  type="button"
                  className={`project${isOpen ? " project--open" : ""}${
                    dropHovered === project.id ? " project--drop" : ""
                  }`}
                  aria-current={isOpen ? "true" : undefined}
                  // The path lives in the title bar, next to the button that
                  // reveals it (PLAN 7.10). This click already means "open
                  // this project", so the row keeps the name and, when the
                  // folder has gone, a warning — the full path stays in the
                  // tooltip.
                  title={
                    project.workspace_exists
                      ? project.workspace_path
                      : `${project.workspace_path} — this folder is not there any more`
                  }
                  onClick={() => void open(project.id)}
                  disabled={busy}
                >
                  <span className="project__name">{project.name}</span>
                  {project.workspace_exists ? null : (
                    <span className="workspace__note">missing</span>
                  )}
                </button>
                <button
                  type="button"
                  className="project__remove"
                  // Removing a project only forgets it here; saying so on the
                  // control itself is cheaper than a confirmation the user
                  // would learn to dismiss without reading.
                  title={`Forget ${project.name}. The folder on disk is not touched.`}
                  aria-label={`Forget project ${project.name}`}
                  onClick={() => void remove(project.id)}
                  disabled={busy}
                >
                  ×
                </button>
              </li>
            );
          })}
        </ul>

        <DropReport />

        <ProjectPicker />
      </Section>

      <ExecHost />

      <SharedFiles />

      <WorldPanel />

      <SessionList />
    </nav>
  );
}
