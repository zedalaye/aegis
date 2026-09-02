/**
 * The application header.
 *
 * Not OS window chrome — the window keeps its native decorations. This is the
 * in-app bar that names the app, shows which workspace is in scope, and offers
 * Hide (when a tray exists) and Quit.
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
 *
 * The board (Phase 17) is the one action here that *is* about a project, and it
 * is here anyway: it is a mode the work area takes over, like Settings, and a
 * button that lived in the rail beside the project rows would read as "open
 * this project" rather than "show me its board".
 *
 * Settings and the board are two modes of one work area, so opening either
 * closes the other. That is decided here rather than in the shell's render
 * chain: a chain that merely preferred one would leave the other's button
 * drawn as pressed with nothing behind it, which is a button that lies.
 *
 * The actions on the right are icon buttons (PLAN 7.10). Each keeps its name
 * as `aria-label` and as a tooltip — never an icon alone. Settings, Audit and
 * Board stay *modes* (`aria-pressed`); Hide and Quit stay actions. The Open
 * control sits on the path, not among them: that click is about the folder,
 * not about a pane of this window.
 */

import { useEffect, useState } from "react";
import type { ReactNode } from "react";

import { appQuit, windowHasTray, windowHide } from "../../ipc/commands";
import { useAudit } from "../../state/audit";
import { useBoard } from "../../state/board";
import { useProjects } from "../../state/projects";
import { useSettings } from "../../state/settings";
import WorkspaceBadge from "../projects/WorkspaceBadge";
import {
  AuditIcon,
  BoardIcon,
  FolderIcon,
  HideIcon,
  QuitIcon,
  SettingsIcon,
} from "./icons";

/** An icon that is also a named button. The name is the tooltip and the label. */
function IconButton({
  label,
  pressed,
  disabled,
  onClick,
  children,
}: {
  readonly label: string;
  readonly pressed?: boolean;
  readonly disabled?: boolean;
  readonly onClick: () => void;
  readonly children: ReactNode;
}) {
  return (
    <button
      type="button"
      className="button button--icon"
      title={label}
      aria-label={label}
      aria-pressed={pressed}
      disabled={disabled}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

export default function TitleBar() {
  const project = useProjects((s) => s.detail?.project ?? null);
  const reveal = useProjects((s) => s.reveal);
  const settingsOpen = useSettings((s) => s.open);
  const openSettings = useSettings((s) => s.openPanel);
  const closeSettings = useSettings((s) => s.closePanel);
  const auditOpen = useAudit((s) => s.open);
  const toggleAudit = useAudit((s) => s.toggleDrawer);
  const boardOpen = useBoard((s) => s.open);
  const openBoard = useBoard((s) => s.openPanel);
  const closeBoard = useBoard((s) => s.closePanel);
  const [hasTray, setHasTray] = useState(true);

  useEffect(() => {
    void windowHasTray()
      .then(setHasTray)
      .catch(() => {
        setHasTray(false);
      });
  }, []);

  return (
    <header className="titlebar">
      <div className="titlebar__identity">
        <span className="titlebar__name">Aegis</span>
        {project === null ? (
          <span className="titlebar__hint">no project open</span>
        ) : (
          <>
            <span className="titlebar__project">{project.name}</span>
            <span className="titlebar__workspace">
              <WorkspaceBadge
                path={project.workspace_path}
                exists={project.workspace_exists}
                maxLength={56}
              />
              <IconButton
                label="Open this folder in the file manager"
                disabled={!project.workspace_exists}
                onClick={() => void reveal(project.id)}
              >
                <FolderIcon />
              </IconButton>
            </span>
          </>
        )}
      </div>

      <div className="titlebar__actions">
        <IconButton
          label="Board"
          pressed={boardOpen}
          // Disabled with nothing open rather than hidden: a board is a fact
          // about a project, and a button that vanished would read as a feature
          // that is not there.
          disabled={project === null}
          onClick={() => {
            if (boardOpen) {
              closeBoard();
            } else {
              closeSettings();
              void openBoard(project?.id ?? null);
            }
          }}
        >
          <BoardIcon />
        </IconButton>
        <IconButton
          label="Audit log"
          pressed={auditOpen}
          onClick={() => void toggleAudit()}
        >
          <AuditIcon />
        </IconButton>
        <IconButton
          label="Settings"
          pressed={settingsOpen}
          onClick={() => {
            if (settingsOpen) {
              closeSettings();
            } else {
              closeBoard();
              void openSettings();
            }
          }}
        >
          <SettingsIcon />
        </IconButton>
        {hasTray ? (
          <IconButton
            label="Hide to tray"
            // A rejection here means the window is already gone, which is what
            // was being asked for; there is nothing useful to report.
            onClick={() => void windowHide().catch(() => {})}
          >
            <HideIcon />
          </IconButton>
        ) : null}
        <IconButton
          label="Quit"
          onClick={() => void appQuit().catch(() => {})}
        >
          <QuitIcon />
        </IconButton>
      </div>
    </header>
  );
}
