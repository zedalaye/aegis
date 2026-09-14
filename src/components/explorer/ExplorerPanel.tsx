/**
 * The workspace explorer (PLAN 7.15): the open project's files, to see.
 *
 * A work-area mode (it needs the width). Read-only except drops: a file dropped
 * on the project or `.aegis/briefs/` is copied there by id; drops on artefacts,
 * the rest of the cabinet or `world/` are refused up front. A drop starts
 * nothing.
 */

import { useEffect } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";

import { on } from "../../ipc/events";
import type { ImportReport } from "../../ipc/bindings";
import { elementAtDrop } from "../../lib/drop";
import { useExplorer } from "../../state/explorer";
import type { DropHint } from "../../state/explorer";
import { useProjects } from "../../state/projects";
import { useSkills } from "../../state/skills";
import { useWorkspace } from "../../state/workspace";

import FileTree from "./FileTree";
import PreviewPane from "./PreviewPane";

/** What a drop at a point of the window does: `undefined` is outside the panel. */
function dropAt(physicalX: number, physicalY: number): string | undefined {
  return elementAtDrop(physicalX, physicalY)?.closest<HTMLElement>("[data-drop]")
    ?.dataset.drop;
}

/** The hover line for a drop target. */
function hintFor(drop: string | undefined): DropHint | null {
  if (drop === undefined) {
    return null;
  }
  if (drop.startsWith("refuse:")) {
    return { accept: false, message: drop.slice("refuse:".length) };
  }
  return {
    accept: true,
    message: "Drop to copy into .aegis/briefs/. The original stays where it is.",
  };
}

/** Whether the open workspace has no `.aegis/briefs/`, as last measured. */
function briefsMissing(): boolean {
  const briefs = useWorkspace
    .getState()
    .layout?.entries.find((entry) => entry.dir === ".aegis/briefs");
  return briefs !== undefined && !briefs.dir_exists;
}

/** What the last drop did, in one or two sentences. */
function Report({ report }: { readonly report: ImportReport }) {
  const arrived = report.arrived.map((arrival) => {
    const landed = arrival.path.slice(arrival.path.lastIndexOf("/") + 1);
    return landed === arrival.from ? arrival.from : `${arrival.from} (as ${landed})`;
  });

  return (
    <>
      {arrived.length === 0 ? null : (
        <span>
          Copied {arrived.join(", ")} into <code>.aegis/briefs/</code>. Nothing
          else happened: no session was started.
        </span>
      )}
      {report.refused.map((refusal) => (
        <span key={refusal.name} className="explorer__refused">
          {refusal.name} was not added: {refusal.reason}.
        </span>
      ))}
    </>
  );
}

export default function ExplorerPanel() {
  const projectId = useProjects((s) => s.detail?.project.id ?? null);
  const close = useExplorer((s) => s.closePanel);
  const followProject = useExplorer((s) => s.followProject);
  const refresh = useExplorer((s) => s.refresh);
  const showIgnored = useExplorer((s) => s.showIgnored);
  const setShowIgnored = useExplorer((s) => s.setShowIgnored);
  const dropHint = useExplorer((s) => s.dropHint);
  const imported = useExplorer((s) => s.imported);
  const dropRefusal = useExplorer((s) => s.dropRefusal);
  const waitingDrop = useExplorer((s) => s.waitingDrop);
  const importing = useExplorer((s) => s.importing);
  const dismissDrop = useExplorer((s) => s.dismissDrop);
  const scaffolding = useWorkspace((s) => s.busy);

  // Follows the open project, the way the board does.
  useEffect(() => {
    void followProject(projectId);
  }, [projectId, followProject]);

  // Drag and drop, only while the panel is up. Two sources, on purpose: the
  // hover line comes from the webview's own drag events, and the drop itself
  // from the runtime's `workspace:dropped`, which is the one that carries an id
  // the runtime will honour.
  useEffect(() => {
    let cancelled = false;
    const detach: (() => void)[] = [];
    const keep = (unlisten: () => void) => {
      if (cancelled) {
        unlisten();
      } else {
        detach.push(unlisten);
      }
    };

    void getCurrentWebview()
      .onDragDropEvent((event) => {
        const { setDropHint } = useExplorer.getState();
        if (event.payload.type === "enter" || event.payload.type === "over") {
          const { x, y } = event.payload.position;
          setDropHint(hintFor(dropAt(x, y)));
        } else {
          setDropHint(null);
        }
      })
      .then(keep)
      .catch(() => {
        // No hover line, and nothing else lost: the drop still arrives below.
      });

    void on("workspace:dropped", (dropped) => {
      // Read the row before touching any state. Clearing the hint re-renders
      // the panel, and a lookup made after that re-render is a lookup against
      // a different page from the one the drop was aimed at.
      const drop = dropAt(dropped.x, dropped.y);
      const explorer = useExplorer.getState();
      explorer.setDropHint(null);

      if (drop === undefined) {
        return;
      }
      const names = dropped.names.join(", ");
      if (drop.startsWith("refuse:")) {
        explorer.refuseDrop(`${names} not added. ${drop.slice("refuse:".length)}`);
        return;
      }
      if (briefsMissing()) {
        explorer.refuseDrop(
          `${names} not added. This workspace has no .aegis/briefs/ yet, and a drop does not lay the shared files down.`,
          dropped.drop_id,
        );
        return;
      }
      void explorer.importDrop(dropped.drop_id);
    })
      .then(keep)
      .catch(() => {});

    return () => {
      cancelled = true;
      while (detach.length > 0) {
        detach.pop()?.();
      }
    };
  }, []);

  if (projectId === null) {
    return (
      <section className="explorer" aria-labelledby="explorer-title">
        <header className="board__header">
          <h1 className="board__title" id="explorer-title">
            Files
          </h1>
          <button type="button" className="button" onClick={close}>
            Close
          </button>
        </header>
        <p className="board__lede">Open a project to see its files.</p>
      </section>
    );
  }

  // Set the shared files up, then add what was dropped. The runtime kept the
  // drop because it checks for `.aegis/briefs/` before claiming one.
  const setUpAndAdd = async () => {
    await useWorkspace.getState().scaffold(projectId);
    void useSkills.getState().loadFor(projectId);
    if (waitingDrop !== null && !briefsMissing()) {
      await useExplorer.getState().importDrop(waitingDrop);
    }
  };

  return (
    <section
      className={`explorer${dropHint === null ? "" : dropHint.accept ? " explorer--accept" : " explorer--refuse"}`}
      aria-labelledby="explorer-title"
      data-drop="brief"
    >
      <header className="board__header">
        <h1 className="board__title" id="explorer-title">
          Files
        </h1>
        <div className="board__controls">
          <label className="explorer__toggle">
            <input
              type="checkbox"
              checked={showIgnored}
              onChange={(event) => void setShowIgnored(event.target.checked)}
            />
            Show ignored
          </label>
          <button type="button" className="button" onClick={() => void refresh()}>
            Refresh
          </button>
          <button type="button" className="button" onClick={close}>
            Close
          </button>
        </div>
      </header>

      <p className="board__lede">
        This project&apos;s folder, read-only. Nothing here saves a file. Drop a
        file onto the project and it is copied into <code>.aegis/briefs/</code>.
      </p>

      {/* An overlay, never in the flow. A line that appeared above the tree
          while dragging pushed every row down by its own height, so the row
          lit up under the cursor was not the row the drop then landed on. */}
      {dropHint === null ? null : (
        <p
          className={`explorer__hint${dropHint.accept ? "" : " explorer__hint--refuse"}`}
          role="status"
        >
          {dropHint.message}
        </p>
      )}

      {importing ? (
        <p className="explorer__report" role="status">
          Copying into <code>.aegis/briefs/</code>…
        </p>
      ) : null}

      {imported === null && dropRefusal === null ? null : (
        <div
          className={`explorer__report${dropRefusal === null ? "" : " explorer__report--refused"}`}
          role="status"
        >
          {imported === null ? null : <Report report={imported} />}
          {dropRefusal === null ? null : <span>{dropRefusal}</span>}
          <span className="explorer__actions">
            {waitingDrop === null ? null : (
              <button
                type="button"
                className="button"
                disabled={scaffolding || importing}
                title="Creates only what is missing, then copies what you dropped into .aegis/briefs/"
                onClick={() => void setUpAndAdd()}
              >
                {scaffolding ? "Setting up…" : "Set up shared files and add it"}
              </button>
            )}
            <button type="button" className="link" onClick={dismissDrop}>
              Dismiss
            </button>
          </span>
        </div>
      )}

      <div className="explorer__body">
        <nav className="explorer__tree" aria-label="Folder">
          <FileTree />
        </nav>
        <PreviewPane projectId={projectId} />
      </div>
    </section>
  );
}
