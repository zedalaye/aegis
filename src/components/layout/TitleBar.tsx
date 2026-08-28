/**
 * The application header.
 *
 * Not OS window chrome — the window keeps its native decorations. This is the
 * in-app bar that names the app, shows which workspace is in scope, and offers
 * the two lifecycle actions.
 *
 * Hide and quit are runtime commands rather than WebView calls: the window has
 * no `core:window` permission, so this bar reaches the window through exactly
 * the same path the tray does.
 */

import { appQuit, windowHide } from "../../ipc/commands";
import { useProjects } from "../../state/projects";
import WorkspaceBadge from "../projects/WorkspaceBadge";

export default function TitleBar() {
  const project = useProjects((s) => s.detail?.project ?? null);

  return (
    <header className="titlebar">
      <div className="titlebar__identity">
        <span className="titlebar__name">Aegis</span>
        {project === null ? (
          <span className="titlebar__hint">no project open</span>
        ) : (
          <>
            <span className="titlebar__project">{project.name}</span>
            <WorkspaceBadge
              path={project.workspace_path}
              exists={project.workspace_exists}
              maxLength={56}
            />
          </>
        )}
      </div>

      <div className="titlebar__actions">
        <button
          type="button"
          className="button"
          // A rejection here means the window is already gone, which is what
          // was being asked for; there is nothing useful to report.
          onClick={() => void windowHide().catch(() => {})}
        >
          Hide to tray
        </button>
        <button
          type="button"
          className="button"
          onClick={() => void appQuit().catch(() => {})}
        >
          Quit
        </button>
      </div>
    </header>
  );
}
