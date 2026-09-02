/**
 * The rail: every project, most recent first, and the open project's sessions
 * beneath them.
 *
 * Two lists in one scrolling column, with the "add a workspace" control pinned
 * at the bottom. The project list is the thing a user changes rarely and the
 * session list the thing they change constantly, which is why the sessions sit
 * closer to the conversation they belong to.
 *
 * The shared-file panel sits between them, on the project's side of that line:
 * `briefs/`, `status/`, `artefacts/` and `decisions/` belong to the folder, not
 * to any one conversation (PLAN 7.3, Phase 11). The world is below it, for the
 * same reason and one stronger: it is the slowest-changing thing here.
 *
 * All four are {@link Section}s, so any of them can be folded away and stays
 * that way. Four stacks is more than a laptop screen holds at once, and which
 * one matters is a question about what somebody is doing this week — the world
 * while founding one, the sessions the rest of the time.
 */

import { useProjects } from "../../state/projects";
import Section from "./Section";
import ProjectPicker from "../projects/ProjectPicker";
import SharedFiles from "../projects/SharedFiles";
import WorldPanel from "../projects/WorldPanel";
import WorkspaceBadge from "../projects/WorkspaceBadge";
import SessionList from "../sessions/SessionList";

export default function Sidebar() {
  const projects = useProjects((s) => s.projects);
  const detail = useProjects((s) => s.detail);
  const status = useProjects((s) => s.status);
  const busy = useProjects((s) => s.busy);
  const open = useProjects((s) => s.open);
  const remove = useProjects((s) => s.remove);

  const openId = detail?.project.id ?? null;

  return (
    <nav className="sidebar" aria-label="Projects and sessions">
      <Section
        id="projects"
        title="Projects"
        badge={
          projects.length === 0 ? null : (
            <span className="rail__count">{projects.length}</span>
          )
        }
      >
        {status === "loading" && projects.length === 0 ? (
          <p className="sidebar__empty">Loading…</p>
        ) : null}

        {status !== "loading" && projects.length === 0 ? (
          <p className="sidebar__empty">
            No projects yet. Add a workspace folder to start.
          </p>
        ) : null}

        <ul className="sidebar__list">
          {projects.map((project) => {
            const isOpen = project.id === openId;
            return (
              <li key={project.id} className="sidebar__item">
                <button
                  type="button"
                  className={`project${isOpen ? " project--open" : ""}`}
                  aria-current={isOpen ? "true" : undefined}
                  onClick={() => void open(project.id)}
                  disabled={busy}
                >
                  <span className="project__name">{project.name}</span>
                  <WorkspaceBadge
                    path={project.workspace_path}
                    exists={project.workspace_exists}
                    maxLength={28}
                  />
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
      </Section>

      <SharedFiles />

      <WorldPanel />

      <SessionList />

      <div className="sidebar__footer">
        <ProjectPicker />
      </div>
    </nav>
  );
}
