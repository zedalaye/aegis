/**
 * Connector state (PLAN 7.3, Phase 18).
 *
 * What external MCP servers are configured, which of them are running, and
 * what each one offers. Three things this store deliberately does *not* do.
 *
 * **It keeps no copy of what is running.** The state, the tool list, the last
 * lines of a server's stderr and the environment variables it is missing all
 * arrive on the row, measured by the runtime. A UI that cached "this one is
 * connected" would be the thing that draws a connector as up after its process
 * has quietly gone.
 *
 * **It does not start anything by itself.** Connectors are started by the Rust
 * runtime at boot, whether or not there is a window; `connector:updated` is how
 * this store hears that one came up, changed what it offers, or died.
 *
 * **It grants nothing.** Installing a connector makes its tools exist. Who may
 * call them is edited on the identity, in the panel above — a separate act, on
 * a different object, exactly as it is for `shell_exec`.
 */

import { create } from "zustand";
import type { UnlistenFn } from "@tauri-apps/api/event";

import type { ConnectorDraft, ConnectorView, ToolInfo } from "../ipc/bindings";
import { subscribe } from "../ipc/events";
import {
  connectorDelete,
  connectorList,
  connectorReconnect,
  connectorSave,
  connectorSetEnabled,
} from "../ipc/commands";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

/** Whether the list has been fetched yet. */
export type LoadStatus = "idle" | "loading" | "ready" | "error";

/** A blank form: enabled, so saving it starts the thing you just described. */
export function blankDraft(): ConnectorDraft {
  return {
    id: "",
    name: "",
    command: "",
    args: [],
    env: [],
    enabled: true,
  };
}

/** The form filled in from an existing connector. */
export function draftOf(view: ConnectorView): ConnectorDraft {
  return {
    id: view.connector.id,
    name: view.connector.name,
    command: view.connector.command,
    args: [...view.connector.args],
    env: [...view.connector.env],
    enabled: view.connector.enabled,
  };
}

export type ConnectorsState = {
  /** Every connector, in the order they were added. */
  readonly connectors: readonly ConnectorView[];
  readonly status: LoadStatus;
  /** True while a save, delete, toggle or reconnect is in flight. */
  readonly busy: boolean;
  /**
   * The connector the form is editing, `"new"` while creating one, or `null`
   * when the form is closed.
   */
  readonly editing: string | null;
  /** What the runtime refused, and which input it was about. */
  readonly fieldError: {
    readonly field: string;
    readonly message: string;
  } | null;
  /** A failure that is not about a form field. */
  readonly error: IpcError | null;

  load: () => Promise<void>;
  /** Replaces one row, from a `connector:updated` event. */
  observed: (view: ConnectorView) => void;
  startNew: () => void;
  startEdit: (connectorId: string) => void;
  cancelEdit: () => void;
  /** Saves the form. Resolves `true` when the runtime accepted it. */
  save: (draft: ConnectorDraft) => Promise<boolean>;
  remove: (connectorId: string) => Promise<void>;
  setEnabled: (connectorId: string, enabled: boolean) => Promise<void>;
  reconnect: (connectorId: string) => Promise<void>;
  dismissError: () => void;
};

/**
 * Splits a rejection into the half that belongs under an input and the half
 * that belongs in the shell's banner, the way the identity and routine forms
 * do. The runtime decides which by setting `field`.
 */
function landing(cause: unknown, command: string) {
  const error = toIpcError(cause, command);
  return error.field === null
    ? { error, fieldError: null }
    : {
        error: null,
        fieldError: { field: error.field, message: error.message },
      };
}

export const useConnectors = create<ConnectorsState>((set, get) => ({
  connectors: [],
  status: "idle",
  busy: false,
  editing: null,
  fieldError: null,
  error: null,

  load: async () => {
    set({ status: "loading" });
    try {
      set({ connectors: await connectorList(), status: "ready", error: null });
    } catch (cause) {
      set({ status: "error", error: toIpcError(cause, "connector_list") });
    }
  },

  // Patched rather than refetched: this arrives whenever a connector settles,
  // which at boot is several times in a row, and a refetch per event would be
  // a list rebuilt for one changed row.
  observed: (view) =>
    set((state) => ({
      connectors: state.connectors.some(
        (held) => held.connector.id === view.connector.id,
      )
        ? state.connectors.map((held) =>
            held.connector.id === view.connector.id ? view : held,
          )
        : [...state.connectors, view],
    })),

  startNew: () => set({ editing: "new", fieldError: null }),
  startEdit: (connectorId) =>
    set({ editing: connectorId, fieldError: null }),
  cancelEdit: () => set({ editing: null, fieldError: null }),

  save: async (draft) => {
    const editing = get().editing;
    if (editing === null) {
      return false;
    }
    const creating = editing === "new";

    set({ busy: true, fieldError: null, error: null });
    try {
      await connectorSave(creating ? null : editing, draft);
      // Refetched rather than patched: the id is editable, so a save can
      // rename the row this store was keying on.
      set({
        connectors: await connectorList(),
        status: "ready",
        editing: null,
      });
      return true;
    } catch (cause) {
      set(landing(cause, "connector_save"));
      return false;
    } finally {
      set({ busy: false });
    }
  },

  remove: async (connectorId) => {
    set({ busy: true, error: null });
    try {
      await connectorDelete(connectorId);
      set({ connectors: await connectorList(), status: "ready" });
      if (get().editing === connectorId) {
        set({ editing: null, fieldError: null });
      }
    } catch (cause) {
      set({ error: toIpcError(cause, "connector_delete") });
    } finally {
      set({ busy: false });
    }
  },

  setEnabled: async (connectorId, enabled) => {
    set({ busy: true, error: null });
    try {
      get().observed(await connectorSetEnabled(connectorId, enabled));
    } catch (cause) {
      set({ error: toIpcError(cause, "connector_set_enabled") });
    } finally {
      set({ busy: false });
    }
  },

  reconnect: async (connectorId) => {
    set({ busy: true, error: null });
    try {
      get().observed(await connectorReconnect(connectorId));
    } catch (cause) {
      set({ error: toIpcError(cause, "connector_reconnect") });
    } finally {
      set({ busy: false });
    }
  },

  dismissError: () => set({ error: null, fieldError: null }),
}));

/**
 * Every connector tool that is callable right now, in connector order.
 *
 * What the identity form offers as things to grant. Only the tools of
 * connectors that are actually up: an allow-list you type is an allow-list you
 * typo, and a checkbox for a tool nothing answers to is a grant nobody can
 * read. A name granted earlier and no longer offered stays on the identity —
 * the runtime keeps it and refuses it — and the form says so.
 */
export function liveTools(connectors: readonly ConnectorView[]): ToolInfo[] {
  return connectors.flatMap((view) => view.tools);
}

/**
 * Keeps the list in step with what the connectors are doing.
 *
 * Like the routine store, this one hears from the runtime while nobody is
 * looking at it: connectors are started at boot, and a server can die at any
 * hour. Patched rather than refetched, one message per changed row.
 */
export function attachConnectorEvents(): Promise<UnlistenFn> {
  return subscribe({
    "connector:updated": (view) => {
      useConnectors.getState().observed(view);
    },
  });
}
