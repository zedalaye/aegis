/**
 * Provider roster state (PLAN 7.19).
 *
 * A cache of the *masked* roster; no key reaches the WebView.
 *
 * - The form edits one row at a time: `selected` names it, or `NEW_ROW` while
 *   adding one.
 * - The draft is separate from the saved values, so a rejected save keeps the
 *   text; `E_INVALID_SETTING` errors land under their field.
 * - The key field is write-only: empty on load, empty means "keep", cleared
 *   after a successful save.
 */

import { create } from "zustand";

import type {
  AuthKind,
  AuthPreset,
  MaskedProvider,
  MaskedSettings,
  ModelCatalog,
  ProviderProbe,
} from "../ipc/bindings";
import {
  settingsAddProvider,
  settingsClearKey,
  settingsDeleteProvider,
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
 * Only the default row reads it.
 */
export const ENV_API_KEY = "AEGIS_API_KEY";

/** Mirrors `DEFAULT_PROVIDER_ID` in `store/agents.rs`: the row that always exists. */
export const DEFAULT_PROVIDER_ID = "default";

/** Mirrors `PROVIDERS_MAX` in `store/settings.rs`. */
export const PROVIDERS_MAX = 16;

/** The `selected` value while a row is being added. */
export const NEW_ROW = "";

/** What the user has typed but not yet saved. */
export type Draft = {
  readonly label: string;
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
  label: "",
  authKind: "api_key",
  baseUrl: "",
  model: "",
  apiKey: "",
};

export type SettingsState = {
  /** The saved roster, masked, or `null` before the first load. */
  readonly settings: MaskedSettings | null;
  readonly status: LoadStatus;
  /** Whether the panel is showing. */
  readonly open: boolean;
  /** The row the form edits, or {@link NEW_ROW}. */
  readonly selected: string;
  /** What is in the form right now. */
  readonly draft: Draft;
  /** True while a command is in flight, so the panel can disable its buttons. */
  readonly busy: boolean;
  /** The last connection test of the selected row, or `null`. */
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

  /** Loads the roster and fills the form from the selected row. */
  load: () => Promise<void>;
  /** Shows the panel, refreshing what it shows. */
  openPanel: () => Promise<void>;
  /** Hides the panel. */
  closePanel: () => void;
  /** Puts one row in the form, or a blank one for {@link NEW_ROW}. */
  select: (providerId: string) => void;
  /** Edits the form. */
  edit: (patch: Partial<Draft>) => void;
  /** Saves the form. Resolves to whether it was accepted. */
  save: () => Promise<boolean>;
  /** Deletes a row. Resolves to whether it was deleted. */
  remove: (providerId: string) => Promise<boolean>;
  /** Removes the selected row's key from the credential store. */
  clearKey: () => Promise<void>;
  /** Asks the runtime to try the selected row's server. */
  runProbe: () => Promise<void>;
  /** Asks the chosen authentication which models it will accept. */
  loadModels: () => Promise<void>;
  /** Applies a `settings:changed` payload from elsewhere. */
  applyChanged: (settings: MaskedSettings) => void;
  /** Clears the last error. */
  dismissError: () => void;
};

/** One row by id. */
export function rowOf(
  settings: MaskedSettings | null,
  providerId: string,
): MaskedProvider | undefined {
  return settings?.providers.find((row) => row.id === providerId);
}

/** The default row, which the runtime always sends first. */
export function defaultRow(settings: MaskedSettings): MaskedProvider | undefined {
  return rowOf(settings, DEFAULT_PROVIDER_ID) ?? settings.providers[0];
}

/**
 * Whether a row, sending `model`, names a real provider.
 *
 * Mirrors `ProviderSettings::is_configured`: a CLI login or Gemini implies
 * its own endpoint, so a model id is enough. An OpenAI-compatible API key
 * still needs a URL and a model.
 */
export function isConfigured(
  row: MaskedProvider,
  model: string = row.model,
): boolean {
  if (model.length === 0) {
    return false;
  }
  return row.auth_kind !== "api_key" || row.base_url.length > 0;
}

/** The prefill values for one authentication kind. */
export function presetOf(
  presets: ReadonlyArray<AuthPreset>,
  kind: AuthKind,
): AuthPreset | undefined {
  return presets.find((item) => item.auth_kind === kind);
}

/**
 * The URL a request would actually hit: the saved one, or the kind's default
 * when the field was left empty.
 */
export function effectiveBaseUrl(
  row: MaskedProvider,
  presets: ReadonlyArray<AuthPreset>,
): string {
  if (row.base_url.length > 0) {
    return row.base_url;
  }
  return presetOf(presets, row.auth_kind)?.default_base_url ?? "";
}

const KIND_NAMES: Readonly<Record<AuthKind, string>> = {
  api_key: "API key",
  gemini: "Gemini",
  claude_cli: "Claude Code",
  codex_cli: "Codex",
  grok_cli: "Grok",
};

/** How a row is named in lists and pickers: its label, else what it is. */
export function providerName(row: MaskedProvider): string {
  if (row.label.length > 0) {
    return row.label;
  }
  const kind = KIND_NAMES[row.auth_kind];
  return row.id === DEFAULT_PROVIDER_ID ? `Default (${kind})` : kind;
}

/** The form as it should look for a saved row, or a blank one. */
function draftOf(row: MaskedProvider | undefined): Draft {
  if (row === undefined) {
    return EMPTY_DRAFT;
  }
  return {
    label: row.label,
    authKind: row.auth_kind,
    baseUrl: row.base_url,
    model: row.model,
    // Never the key: there is nothing to prefill it with, and an empty field
    // is exactly what "leave the stored key alone" means on the way back.
    apiKey: "",
  };
}

export const useSettings = create<SettingsState>((set, get) => {
  /**
   * Runs a command, routing a field refusal under its input and anything else
   * to the banner; both are cleared first. On success the form is refilled
   * from the row `selectAfter` names (default: the selected one).
   */
  const guard = async (
    command: string,
    run: () => Promise<MaskedSettings>,
    selectAfter?: (settings: MaskedSettings) => string,
  ): Promise<boolean> => {
    set({ busy: true, error: null, fieldError: null });
    try {
      const settings = await run();
      const wanted = selectAfter?.(settings) ?? get().selected;
      // A row deleted elsewhere falls back to the default one.
      const selected =
        wanted === NEW_ROW || rowOf(settings, wanted) !== undefined
          ? wanted
          : DEFAULT_PROVIDER_ID;
      set({
        settings,
        status: "ready",
        selected,
        draft: draftOf(rowOf(settings, selected)),
      });
      return true;
    } catch (cause) {
      const error = toIpcError(cause, command);
      if (error.field !== null) {
        set({ fieldError: { field: error.field, message: error.message } });
      } else {
        set({ error });
      }

      // The row may be saved even if the key was not: re-read, keeping the
      // draft.
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
    selected: DEFAULT_PROVIDER_ID,
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
      // Refetched on every open rather than trusted from the last one: a key
      // can have been added or removed outside Aegis since, and the panel's
      // whole job is to report what is actually there.
      set({ open: true, probe: null });
      await get().load();
    },

    closePanel: () => set({ open: false, fieldError: null }),

    select: (providerId) =>
      set({
        selected: providerId,
        draft: draftOf(rowOf(get().settings, providerId)),
        probe: null,
        fieldError: null,
        models: [],
        modelsLive: false,
        modelsMessage: "",
      }),

    edit: (patch) => set({ draft: { ...get().draft, ...patch } }),

    save: async () => {
      const { selected, draft } = get();
      const before = new Set(get().settings?.providers.map((row) => row.id));
      const saved =
        selected === NEW_ROW
          ? await guard(
              "settings_add_provider",
              () => settingsAddProvider(draft),
              // The added row is the one id the roster did not have.
              (settings) =>
                settings.providers.find((row) => !before.has(row.id))?.id ??
                DEFAULT_PROVIDER_ID,
            )
          : await guard("settings_set", () => settingsSet(selected, draft));

      // A stale result from the last configuration would be worse than none:
      // it describes a server this one may no longer point at.
      if (saved) {
        set({ probe: null });
      }
      return saved;
    },

    remove: async (providerId) =>
      guard(
        "settings_delete_provider",
        () => settingsDeleteProvider(providerId),
        () =>
          get().selected === providerId ? DEFAULT_PROVIDER_ID : get().selected,
      ),

    clearKey: async () => {
      const { selected } = get();
      if (selected === NEW_ROW) {
        return;
      }
      await guard("settings_clear_key", () => settingsClearKey(selected));
    },

    runProbe: async () => {
      const { selected } = get();
      if (selected === NEW_ROW) {
        return;
      }
      set({ probing: true, error: null });
      try {
        set({ probe: await settingsProbeProvider(selected) });
      } catch (cause) {
        set({ error: toIpcError(cause, "settings_probe_provider") });
      } finally {
        set({ probing: false });
      }
    },

    loadModels: async () => {
      const { selected, draft } = get();
      set({ modelsBusy: true, modelsMessage: "" });
      try {
        const catalog: ModelCatalog = await settingsListModels(
          draft.authKind,
          draft.baseUrl,
          selected === NEW_ROW ? undefined : selected,
        );
        // Dropped if the user moved to another row meanwhile.
        if (get().selected !== selected) {
          return;
        }
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
      const { selected, draft } = get();
      const saved = rowOf(get().settings, selected);
      const next = rowOf(settings, selected);
      // The form is only refilled when the user is not in the middle of
      // editing it: an event arriving mid-typing must not take the text away.
      const untouched =
        selected !== NEW_ROW &&
        draft.apiKey.length === 0 &&
        draft.authKind === saved?.auth_kind;
      const gone = selected !== NEW_ROW && next === undefined;
      set({
        settings,
        status: "ready",
        ...(gone
          ? {
              selected: DEFAULT_PROVIDER_ID,
              draft: draftOf(rowOf(settings, DEFAULT_PROVIDER_ID)),
            }
          : untouched
            ? { draft: draftOf(next) }
            : {}),
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
