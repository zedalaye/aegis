/**
 * Dropping a file onto a project in the list (PLAN 7.15).
 *
 * Works for any project row, landing in its `.aegis/briefs/` (refused without
 * one). The explorer's listener covers a disjoint area. Drops are named by id,
 * never by path.
 */

import { create } from "zustand";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";

import type { ImportReport } from "../ipc/bindings";
import { workspaceImportBrief } from "../ipc/commands";
import { on } from "../ipc/events";
import { elementAtDrop } from "../lib/drop";
import { toIpcError } from "../lib/errors";
import { useExplorer } from "./explorer";

/** The project row under a drop point, by id, or `null`. */
function projectAt(physicalX: number, physicalY: number): string | null {
  return (
    elementAtDrop(physicalX, physicalY)?.closest<HTMLElement>("[data-drop-project]")
      ?.dataset.dropProject ?? null
  );
}

export type ProjectDropState = {
  /** The project row a drag is hovering, while one is. */
  readonly hovered: string | null;
  /** The project a drop is being copied into. */
  readonly importing: string | null;
  /** What the last drop onto a project did. */
  readonly result: {
    readonly projectId: string;
    readonly report: ImportReport;
  } | null;
  /** Why the last drop onto a project was not taken. */
  readonly refusal: {
    readonly projectId: string;
    readonly names: readonly string[];
    readonly message: string;
  } | null;

  setHovered: (projectId: string | null) => void;
  importInto: (
    projectId: string,
    dropId: string,
    names: readonly string[],
  ) => Promise<void>;
  dismiss: () => void;
};

export const useProjectDrops = create<ProjectDropState>((set, get) => ({
  hovered: null,
  importing: null,
  result: null,
  refusal: null,

  setHovered: (hovered) => {
    if (get().hovered !== hovered) {
      set({ hovered });
    }
  },

  importInto: async (projectId, dropId, names) => {
    set({ importing: projectId, result: null, refusal: null });
    try {
      const report = await workspaceImportBrief(projectId, dropId);
      set({ result: { projectId, report } });
      // A Files panel open on that project is showing a folder that just
      // changed.
      const explorer = useExplorer.getState();
      if (explorer.open && explorer.projectId === projectId) {
        void explorer.refresh();
      }
    } catch (cause) {
      set({
        refusal: {
          projectId,
          names,
          message: toIpcError(cause, "workspace_import_brief").message,
        },
      });
    } finally {
      set({ importing: null });
    }
  },

  dismiss: () => set({ result: null, refusal: null }),
}));

/**
 * Listens for drops onto project rows, for the life of the window.
 *
 * The row under a drop is read before anything is set, for the reason
 * `elementAtDrop` gives. A drop that lands anywhere but a project row is left
 * alone here.
 */
export async function attachProjectDropEvents(): Promise<UnlistenFn> {
  const hover = await getCurrentWebview()
    .onDragDropEvent((event) => {
      const { setHovered } = useProjectDrops.getState();
      if (event.payload.type === "enter" || event.payload.type === "over") {
        const { x, y } = event.payload.position;
        setHovered(projectAt(x, y));
      } else {
        setHovered(null);
      }
    })
    // No outline while dragging, and nothing else lost: the drop still
    // arrives below.
    .catch((): UnlistenFn => () => {});

  const dropped = await on("workspace:dropped", (drop) => {
    const projectId = projectAt(drop.x, drop.y);
    const drops = useProjectDrops.getState();
    drops.setHovered(null);
    if (projectId !== null) {
      void drops.importInto(projectId, drop.drop_id, drop.names);
    }
  });

  return () => {
    hover();
    dropped();
  };
}
