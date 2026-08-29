/**
 * Provider settings state.
 *
 * The runtime owns the truth and this is a cache of the *masked* view of it.
 * There is no unmasked counterpart anywhere in the WebView: the key is in the
 * OS credential store or in the environment, and what this store holds is a
 * source, a hint of four characters, and whether the machine has a credential
 * store at all.
 *
 * Two consequences shape the shape of it.
 *
 * The draft the user is typing is kept separately from the saved settings, so
 * a rejected save leaves the text in the form to be corrected rather than
 * snapping back to what is on disk. `E_INVALID_SETTING` carries the field it
 * is about, which is what lets the message land under the right input instead
 * of in a banner over the whole panel.
 *
 * And the key field is write-only. It starts empty on every load, an empty
 * value means "leave the stored key alone", and it is cleared the moment a
 * save succeeds — a form that redisplayed the key would be a form that had
 * been given it.
 *
 * Errors are held rather than thrown, as in the other stores: every action
 * resolves, and a failure lands where the panel can render it.
 */

import { create } from "zustand";

import type { MaskedSettings, ProviderProbe } from "../ipc/bindings";
import {
  settingsClearKey,
  settingsGet,
  settingsProbeProvider,
  settingsSet,
} from "../ipc/commands";
import { subscribe } from "../ipc/events";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";

/** Whether the settings have been loaded yet. */
export type LoadStatus = "idle" | "loading" | "ready" | "error";

/**
 * The environment variable the runtime reads when the credential store holds
 * nothing.
 *
 * Named in the panel so a user on a machine with no keyring knows what to set.
 * It is `ENV_API_KEY` in `src-tauri/src/secrets.rs`; the two are spelled here
 * and there because a constant is not a type and does not cross `ts-rs`.
 */
export const ENV_API_KEY = "AEGIS_API_KEY";

/** What the user has typed but not yet saved. */
export type Draft = {
  readonly baseUrl: string;
  readonly model: string;
  /** Write-only. Empty means "keep whatever key is stored". */
  readonly apiKey: string;
};

/** A refusal that belongs under one input rather than over the whole form. */
export type FieldError = {
  /** The runtime's own name for the input — "base URL", "model". */
  readonly field: string;
  readonly message: string;
};

const EMPTY_DRAFT: Draft = { baseUrl: "", model: "", apiKey: "" };

export type SettingsState = {
  /** The saved settings, masked, or `null` before the first load. */
  readonly settings: MaskedSettings | null;
  readonly status: LoadStatus;
  /** Whether the panel is showing. */
  readonly open: boolean;
  /** What is in the form right now. */
  readonly draft: Draft;
  /** True while a command is in flight, so the panel can disable its buttons. */
  readonly busy: boolean;
  /** The last connection test, or `null` if none has been run. */
  readonly probe: ProviderProbe | null;
  readonly probing: boolean;
  /** A refusal about one input. */
  readonly fieldError: FieldError | null;
  /** Any other failure. */
  readonly error: IpcError | null;

  /** Loads the settings and fills the form from them. */
  load: () => Promise<void>;
  /** Shows the panel, refreshing what it shows. */
  openPanel: () => Promise<void>;
  /** Hides the panel. */
  closePanel: () => void;
  /** Edits the form. */
  edit: (patch: Partial<Draft>) => void;
  /** Saves the form. Resolves to whether it was accepted. */
  save: () => Promise<boolean>;
  /** Removes the key from the credential store. */
  clearKey: () => Promise<void>;
  /** Asks the runtime to try the configured server. */
  runProbe: () => Promise<void>;
  /** Applies a `settings:changed` payload from elsewhere. */
  applyChanged: (settings: MaskedSettings) => void;
  /** Clears the last error. */
  dismissError: () => void;
};

/** The form as it should look for these saved settings. */
function draftOf(settings: MaskedSettings): Draft {
  return {
    baseUrl: settings.base_url,
    model: settings.model,
    // Never the key: there is nothing to prefill it with, and an empty field
    // is exactly what "leave the stored key alone" means on the way back.
    apiKey: "",
  };
}

export const useSettings = create<SettingsState>((set, get) => {
  /**
   * Runs a command, sorting its failure into the right place.
   *
   * A refusal that names a field belongs under that input; everything else is
   * a banner. Both are cleared on entry, so a second attempt does not show the
   * first attempt's complaint.
   */
  const guard = async (
    command: string,
    run: () => Promise<MaskedSettings>,
  ): Promise<boolean> => {
    set({ busy: true, error: null, fieldError: null });
    try {
      const settings = await run();
      set({ settings, status: "ready", draft: draftOf(settings) });
      return true;
    } catch (cause) {
      const error = toIpcError(cause, command);
      if (error.field !== null) {
        set({ fieldError: { field: error.field, message: error.message } });
      } else {
        set({ error });
      }

      // A save is two steps in the runtime — the document, then the key — and
      // the second can fail on a machine with no credential store after the
      // first has already been written. Asking again is how the panel ends up
      // showing what is really stored rather than what it had before. The
      // draft is left alone: the user is mid-correction.
      if (command !== "settings_get") {
        try {
          set({ settings: await settingsGet() });
        } catch {
          // Nothing better to say than the failure already being reported.
        }
      }
      return false;
    } finally {
      set({ busy: false });
    }
  };

  return {
    settings: null,
    status: "idle",
    open: false,
    draft: EMPTY_DRAFT,
    busy: false,
    probe: null,
    probing: false,
    fieldError: null,
    error: null,

    load: async () => {
      set({ status: "loading" });
      if (!(await guard("settings_get", settingsGet))) {
        set({ status: "error" });
      }
    },

    openPanel: async () => {
      // Refetched on every open rather than trusted from the last one: the key
      // can have been added or removed outside Aegis since, and the panel's
      // whole job is to report what is actually there.
      set({ open: true, probe: null });
      await get().load();
    },

    closePanel: () => set({ open: false, fieldError: null }),

    edit: (patch) => set({ draft: { ...get().draft, ...patch } }),

    save: async () => {
      const { baseUrl, model, apiKey } = get().draft;
      const saved = await guard("settings_set", () =>
        settingsSet(baseUrl, model, apiKey),
      );

      // A stale result from the last configuration would be worse than none:
      // it describes a server this one may no longer point at.
      if (saved) {
        set({ probe: null });
      }
      return saved;
    },

    clearKey: async () => {
      await guard("settings_clear_key", settingsClearKey);
    },

    runProbe: async () => {
      set({ probing: true, error: null });
      try {
        set({ probe: await settingsProbeProvider() });
      } catch (cause) {
        set({ error: toIpcError(cause, "settings_probe_provider") });
      } finally {
        set({ probing: false });
      }
    },

    applyChanged: (settings) => {
      // The form is only refilled when the user is not in the middle of
      // editing it: an event arriving mid-typing must not take the text away.
      const untouched = get().draft.apiKey.length === 0;
      set({
        settings,
        status: "ready",
        ...(untouched ? { draft: draftOf(settings) } : {}),
      });
    },

    dismissError: () => set({ error: null, fieldError: null }),
  };
});

/**
 * Subscribes the store to `settings:changed`.
 *
 * One listener for the app, attached by `AppShell` beside the others. The
 * event carries the same masked payload the commands return, so a change made
 * anywhere — including by a future scheduler with no window open — lands here
 * without a refetch.
 */
export function attachSettingsEvents(): Promise<UnlistenFn> {
  return subscribe({
    "settings:changed": (settings) => {
      useSettings.getState().applyChanged(settings);
    },
  });
}
