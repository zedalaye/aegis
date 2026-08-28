/**
 * The project rail: every project, most recent first, plus the way to add one.
 *
 * Sessions will live under the open project here from Phase 5; the pane is
 * laid out for two lists so that addition does not move anything.
 */

import { useProjects } from "../../state/projects";
import ProjectPicker from "../projects/ProjectPicker";
import WorkspaceBadge from "../projects/WorkspaceBadge";

export default function Sidebar() {
  const projects = useProjects((s) => s.projects);
  const detail = useProjects((s) => s.detail);
  const status = useProjects((s) => s.status);
  const busy = useProjects((s) => s.busy);
  const open = useProjects((s) => s.open);
  const remove = useProjects((s) => s.remove);

  const openId = detail?.project.id ?? null;

  return (
    <nav className="sidebar" aria-label="Projects">
      <h2 className="sidebar__heading">Projects</h2>

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

      <div className="sidebar__footer">
        <ProjectPicker />
      </div>
    </nav>
  );
}
