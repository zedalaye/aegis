/**
 * The application header.
 *
 * The in-app header (native decorations are kept): workspace in scope, Hide
 * (with a tray) and Quit via runtime commands, since the WebView has no
 * `core:window` permission.
 *
 * Settings, Board and Files are mutually exclusive work-area modes, enforced
 * here so no button shows pressed with nothing behind it; the audit drawer is
 * app-wide. Icon buttons keep `aria-label` and a tooltip (PLAN 7.10); modes use
 * `aria-pressed`.
 */

import { useEffect, useState } from "react";
import type { ReactNode } from "react";

import { appQuit, windowHasTray, windowHide } from "../../ipc/commands";
import { useAudit } from "../../state/audit";
import { useBoard } from "../../state/board";
import { useExplorer } from "../../state/explorer";
import { useProjects } from "../../state/projects";
import { useSettings } from "../../state/settings";
import WorkspaceBadge from "../projects/WorkspaceBadge";
import {
  AuditIcon,
  BoardIcon,
  FilesIcon,
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
  badge = 0,
  onClick,
  children,
}: {
  readonly label: string;
  readonly pressed?: boolean;
  readonly disabled?: boolean;
  /** A count worn on the button; zero wears nothing. */
  readonly badge?: number;
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
      {badge > 0 ? (
        <span className="button__badge" aria-hidden="true">
          {badge > 9 ? "9+" : badge}
        </span>
      ) : null}
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
  const waiting = useBoard((s) => s.waiting);
  const openBoard = useBoard((s) => s.openPanel);
  const closeBoard = useBoard((s) => s.closePanel);
  const filesOpen = useExplorer((s) => s.open);
  const openFiles = useExplorer((s) => s.openPanel);
  const closeFiles = useExplorer((s) => s.closePanel);
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
          label="Files"
          pressed={filesOpen}
          // Disabled with nothing open, for the board's reason: the files are
          // a project's, and there is no folder to show without one.
          disabled={project === null}
          onClick={() => {
            if (filesOpen) {
              closeFiles();
            } else {
              closeSettings();
              closeBoard();
              void openFiles(project?.id ?? null);
            }
          }}
        >
          <FilesIcon />
        </IconButton>
        <IconButton
          // The label carries the count as well as the badge: a number drawn
          // beside an icon is not something a screen reader announces, and
          // "something is waiting for you" is the whole point of it.
          label={
            waiting === 0
              ? "Board"
              : `Board — ${waiting} call${waiting === 1 ? "" : "s"} parked for you`
          }
          badge={waiting}
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
              closeFiles();
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
              closeFiles();
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
