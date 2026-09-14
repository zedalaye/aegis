/**
 * Explorer state (PLAN 7.15).
 *
 * Expanded folders, the previewed file and the last drop's result, refetched
 * rather than patched, and only while the panel is open. Folders list on
 * expand. Nothing writes files except `importDrop`, which names a drop by id.
 * Preview errors stay in the pane.
 */

import { create } from "zustand";
import type { UnlistenFn } from "@tauri-apps/api/event";

import type { FilePreview, ImportReport, TreeListing } from "../ipc/bindings";
import {
  workspaceImportBrief,
  workspacePreview,
  workspaceTree,
} from "../ipc/commands";
import { subscribe } from "../ipc/events";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

/** Whether a preview has been read yet. */
export type LoadStatus = "idle" | "loading" | "ready" | "error";

/** Where a drag is hovering, and what dropping there would do. */
export type DropHint = {
  /** Whether a drop here lands in `.aegis/briefs/`. */
  readonly accept: boolean;
  /** What to say about it. */
  readonly message: string;
};

/** The folders above a path, outermost first: `a/b/c` → `a`, `a/b`. */
function ancestors(path: string): string[] {
  const segments = path.split("/");
  return segments.slice(0, -1).map((_, index) => segments.slice(0, index + 1).join("/"));
}

export type ExplorerState = {
  /** Whether the panel is showing. */
  readonly open: boolean;
  /** The project the tree belongs to, or `null`. */
  readonly projectId: string | null;
  /** Whether ignored entries are listed, marked, instead of counted. */
  readonly showIgnored: boolean;
  /** Every folder listed so far, by workspace-relative path; `""` is the root. */
  readonly listings: Readonly<Record<string, TreeListing>>;
  /** Folders drawn open. The root is always open and is not in here. */
  readonly expanded: readonly string[];
  /** The file being previewed. */
  readonly selected: string | null;
  readonly preview: FilePreview | null;
  readonly previewStatus: LoadStatus;
  /** Why the selected file could not be previewed. Drawn in the pane. */
  readonly previewError: IpcError | null;
  /** What a drag hovering over the panel would do, while one is. */
  readonly dropHint: DropHint | null;
  /** What the last drop did. */
  readonly imported: ImportReport | null;
  /**
   * Why the last drop was not taken, when the window decided before asking —
   * a drop onto `.aegis/artefacts/` or `world/`, or onto a workspace with no
   * `.aegis/briefs/` yet.
   */
  readonly dropRefusal: string | null;
  /**
   * A drop refused only because `.aegis/briefs/` is missing. The runtime keeps
   * it for a couple of minutes, so setting up the shared files can still add
   * it.
   */
  readonly waitingDrop: string | null;
  readonly importing: boolean;
  /** A failure worth the banner: the tree itself would not load. */
  readonly error: IpcError | null;

  openPanel: (projectId: string | null) => Promise<void>;
  closePanel: () => void;
  /** Follows the open project; a no-op while the panel is closed. */
  followProject: (projectId: string | null) => Promise<void>;
  /** Opens a folder, or folds it. */
  toggleDir: (path: string) => Promise<void>;
  /** Previews a file, opening the folders above it. */
  select: (path: string) => Promise<void>;
  /** Previews the first candidate that exists: a path a document named. */
  openPath: (candidates: readonly string[]) => Promise<void>;
  setShowIgnored: (show: boolean) => Promise<void>;
  /** Re-reads every open folder and the preview, in place. */
  refresh: () => Promise<void>;
  setDropHint: (hint: DropHint | null) => void;
  /** Copies a held drop into `.aegis/briefs/`. */
  importDrop: (dropId: string) => Promise<void>;
  /** Says a drop was not taken, without asking the runtime. */
  refuseDrop: (message: string, waitingDrop?: string) => void;
  dismissDrop: () => void;
  dismissError: () => void;
};

export const useExplorer = create<ExplorerState>((set, get) => {
  /** Lists one folder into `listings`, for the project and view it was asked for. */
  const load = async (dir: string): Promise<boolean> => {
    const { projectId, showIgnored } = get();
    if (projectId === null) {
      return false;
    }
    try {
      const listing = await workspaceTree(projectId, dir, showIgnored);
      // Guarded: a slow listing that lands after the project or the view
      // changed must not draw one folder's rows under another's.
      if (get().projectId === projectId && get().showIgnored === showIgnored) {
        set({ listings: { ...get().listings, [dir]: listing } });
      }
      return true;
    } catch (cause) {
      if (dir === "") {
        set({ error: toIpcError(cause, "workspace_tree") });
      }
      // A folder that has gone since it was opened is folded away rather
      // than drawn as an error row.
      const { [dir]: _gone, ...rest } = get().listings;
      set({
        listings: rest,
        expanded: get().expanded.filter((path) => path !== dir),
      });
      return false;
    }
  };

  /** Reads the preview of `path`, quietly when `quiet` (a refresh). */
  const read = async (path: string, quiet: boolean): Promise<boolean> => {
    const projectId = get().projectId;
    if (projectId === null) {
      return false;
    }
    if (!quiet) {
      set({ previewStatus: "loading", previewError: null });
    }
    try {
      const preview = await workspacePreview(projectId, path);
      if (get().projectId === projectId && get().selected === path) {
        set({ preview, previewStatus: "ready", previewError: null });
      }
      return true;
    } catch (cause) {
      if (get().selected === path) {
        set({
          preview: null,
          previewStatus: "error",
          previewError: toIpcError(cause, "workspace_preview"),
        });
      }
      return false;
    }
  };

  const reset = {
    listings: {},
    expanded: [],
    selected: null,
    preview: null,
    previewStatus: "idle" as const,
    previewError: null,
    dropHint: null,
    imported: null,
    dropRefusal: null,
    waitingDrop: null,
  };

  return {
    open: false,
    projectId: null,
    showIgnored: false,
    importing: false,
    error: null,
    ...reset,

    openPanel: async (projectId) => {
      set({ open: true, projectId, ...reset, error: null });
      if (projectId !== null) {
        await load("");
      }
    },

    closePanel: () => set({ open: false, projectId: null, ...reset }),

    followProject: async (projectId) => {
      if (!get().open || get().projectId === projectId) {
        return;
      }
      await get().openPanel(projectId);
    },

    toggleDir: async (path) => {
      if (get().expanded.includes(path)) {
        set({ expanded: get().expanded.filter((held) => held !== path) });
        return;
      }
      set({ expanded: [...get().expanded, path] });
      await load(path);
    },

    select: async (path) => {
      const above = ancestors(path);
      set({
        selected: path,
        expanded: [...new Set([...get().expanded, ...above])],
      });
      await Promise.all([
        read(path, false),
        ...above.filter((dir) => get().listings[dir] === undefined).map(load),
      ]);
    },

    openPath: async (candidates) => {
      for (const candidate of candidates) {
        const projectId = get().projectId;
        if (projectId === null) {
          return;
        }
        try {
          await workspacePreview(projectId, candidate);
        } catch {
          continue;
        }
        await get().select(candidate);
        return;
      }
      // None of them is there. Selecting the first makes the pane say so,
      // in the runtime's words, where the file would have been.
      const first = candidates[0];
      if (first !== undefined) {
        await get().select(first);
      }
    },

    setShowIgnored: async (showIgnored) => {
      if (get().showIgnored === showIgnored) {
        return;
      }
      set({ showIgnored, listings: {} });
      await Promise.all(["", ...get().expanded].map(load));
    },

    refresh: async () => {
      const { open, projectId, expanded, selected } = get();
      if (!open || projectId === null) {
        return;
      }
      await Promise.all([
        ...["", ...expanded].map(load),
        ...(selected === null ? [] : [read(selected, true)]),
      ]);
    },

    setDropHint: (dropHint) => set({ dropHint }),

    importDrop: async (dropId) => {
      const projectId = get().projectId;
      if (projectId === null) {
        return;
      }
      set({ importing: true, dropRefusal: null, imported: null, dropHint: null });
      try {
        const report = await workspaceImportBrief(projectId, dropId);
        set({ imported: report, waitingDrop: null });
        // Opened where it landed, so the answer to "did it arrive" is the
        // file itself rather than a sentence about it.
        const first = report.arrived[0];
        await get().refresh();
        if (first !== undefined) {
          await get().select(first.path);
        }
      } catch (cause) {
        set({ dropRefusal: toIpcError(cause, "workspace_import_brief").message });
      } finally {
        set({ importing: false });
      }
    },

    refuseDrop: (message, waitingDrop) =>
      set({
        dropRefusal: message,
        waitingDrop: waitingDrop ?? null,
        imported: null,
        dropHint: null,
      }),

    dismissDrop: () => set({ imported: null, dropRefusal: null, waitingDrop: null }),

    dismissError: () => set({ error: null }),
  };
});

/**
 * Re-reads the open tree on `turn:finished` only, so it does not jump after
 * every tool call.
 */
export function attachExplorerEvents(): Promise<UnlistenFn> {
  return subscribe({
    "turn:finished": () => {
      void useExplorer.getState().refresh();
    },
  });
}
