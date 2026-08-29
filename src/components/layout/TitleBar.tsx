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
 *
 * Settings is here rather than in the sidebar because it is not about a
 * project: where the model comes from is a fact about the application, and it
 * has to be reachable on a fresh install where there is no project yet. The
 * audit log is here for the same reason and one more: the record covers every
 * session including deleted ones, so it does not belong under any of them.
 */

import { appQuit, windowHide } from "../../ipc/commands";
import { useAudit } from "../../state/audit";
import { useProjects } from "../../state/projects";
import { useSettings } from "../../state/settings";
import WorkspaceBadge from "../projects/WorkspaceBadge";

export default function TitleBar() {
  const project = useProjects((s) => s.detail?.project ?? null);
  const settingsOpen = useSettings((s) => s.open);
  const openSettings = useSettings((s) => s.openPanel);
  const closeSettings = useSettings((s) => s.closePanel);
  const auditOpen = useAudit((s) => s.open);
  const toggleAudit = useAudit((s) => s.toggleDrawer);

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
          aria-pressed={auditOpen}
          onClick={() => void toggleAudit()}
        >
          Audit log
        </button>
        <button
          type="button"
          className="button"
          aria-pressed={settingsOpen}
          onClick={() => {
            if (settingsOpen) {
              closeSettings();
            } else {
              void openSettings();
            }
          }}
        >
          Settings
        </button>
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
