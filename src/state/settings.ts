/**
 * Provider settings state.
 *
 * A cache of the *masked* settings; the key never reaches the WebView.
 *
 * - The draft is separate from the saved values, so a rejected save keeps the
 *   text; `E_INVALID_SETTING` errors land under their field.
 * - The key field is write-only: empty on load, empty means "keep", cleared
 *   after a successful save.
 */

import { create } from "zustand";

import type {
  AuthKind,
  MaskedSettings,
  ModelCatalog,
  ProviderProbe,
} from "../ipc/bindings";
import {
  settingsClearKey,
  settingsGet,
  settingsListModels,
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
 * The key's environment variable, shown when there is no keyring. Must match
 * `ENV_API_KEY` in `src-tauri/src/secrets.rs` (constants do not cross `ts-rs`).
 */
export const ENV_API_KEY = "AEGIS_API_KEY";

/** What the user has typed but not yet saved. */
export type Draft = {
  readonly authKind: AuthKind;
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

const EMPTY_DRAFT: Draft = {
  authKind: "api_key",
  baseUrl: "",
  model: "",
  apiKey: "",
};

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
  /** Model ids the picker can offer. Empty until the first fetch. */
  readonly models: ReadonlyArray<string>;
  /** Whether `models` came from the provider just now. */
  readonly modelsLive: boolean;
  /** Why a live list was not used, when it was not. */
  readonly modelsMessage: string;
  /** True while a models fetch is in flight. */
  readonly modelsBusy: boolean;

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
  /** Asks the chosen authentication which models it will accept. */
  loadModels: () => Promise<void>;
  /** Applies a `settings:changed` payload from elsewhere. */
  applyChanged: (settings: MaskedSettings) => void;
  /** Clears the last error. */
  dismissError: () => void;
};

/**
 * Whether these settings name a real provider.
 *
 * Mirrors `ProviderSettings::is_configured`: a CLI login or Gemini implies
 * its own endpoint, so a model id is enough. An OpenAI-compatible API key
 * still needs a URL and a model.
 */
export function isConfigured(settings: MaskedSettings): boolean {
  if (settings.model.length === 0) {
    return false;
  }
  return settings.auth_kind !== "api_key" || settings.base_url.length > 0;
}

/**
 * The URL a request would actually hit: the saved one, or the CLI default
 * when the field was left empty.
 */
export function effectiveBaseUrl(settings: MaskedSettings): string {
  if (settings.base_url.length > 0) {
    return settings.base_url;
  }
  const preset = settings.presets.find(
    (item) => item.auth_kind === settings.auth_kind,
  );
  return preset?.default_base_url ?? "";
}

/** The form as it should look for these saved settings. */
function draftOf(settings: MaskedSettings): Draft {
  return {
    authKind: settings.auth_kind,
    baseUrl: settings.base_url,
    model: settings.model,
    // Never the key: there is nothing to prefill it with, and an empty field
    // is exactly what "leave the stored key alone" means on the way back.
    apiKey: "",
  };
}

export const useSettings = create<SettingsState>((set, get) => {
  /**
   * Runs a command, routing a field refusal under its input and anything else
   * to the banner; both are cleared first.
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

      // The document may be saved even if the key was not: re-read, keeping
      // the draft.
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
    models: [],
    modelsLive: false,
    modelsMessage: "",
    modelsBusy: false,

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
      const { authKind, baseUrl, model, apiKey } = get().draft;
      const saved = await guard("settings_set", () =>
        settingsSet(baseUrl, model, apiKey, authKind),
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

    loadModels: async () => {
      const { authKind, baseUrl } = get().draft;
      set({ modelsBusy: true, modelsMessage: "" });
      try {
        const catalog: ModelCatalog = await settingsListModels(authKind, baseUrl);
        const current = get().draft.model;
        const first = catalog.models[0];
        const nextModel =
          current.length === 0 && first !== undefined ? first : current;
        set({
          models: catalog.models,
          modelsLive: catalog.live,
          modelsMessage: catalog.message,
          ...(nextModel === current
            ? {}
            : { draft: { ...get().draft, model: nextModel } }),
        });
      } catch (cause) {
        set({
          modelsLive: false,
          modelsMessage: toIpcError(cause, "settings_list_models").message,
        });
      } finally {
        set({ modelsBusy: false });
      }
    },

    applyChanged: (settings) => {
      // The form is only refilled when the user is not in the middle of
      // editing it: an event arriving mid-typing must not take the text away.
      const untouched =
        get().draft.apiKey.length === 0 &&
        get().draft.authKind === get().settings?.auth_kind;
      set({
        settings,
        status: "ready",
        ...(untouched ? { draft: draftOf(settings) } : {}),
      });
    },

    dismissError: () => set({ error: null, fieldError: null }),
  };
});

/** Subscribes the store to `settings:changed` (attached by `AppShell`). */
export function attachSettingsEvents(): Promise<UnlistenFn> {
  return subscribe({
    "settings:changed": (settings) => {
      useSettings.getState().applyChanged(settings);
    },
  });
}
