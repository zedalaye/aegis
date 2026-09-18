/**
 * The decision model's form (PLAN 7.18): TypeSafe Jev, which is not a
 * provider row.
 *
 * The saved values come from the masked settings in {@link useSettings}; this
 * store holds only the draft, the probe and the errors. The key field is
 * write-only, as for providers.
 */

import { create } from "zustand";

import type { MaskedDecision, ProviderProbe } from "../ipc/bindings";
import {
  settingsClearDecisionKey,
  settingsProbeDecision,
  settingsSetDecision,
} from "../ipc/commands";
import { toIpcError } from "../lib/errors";
import type { IpcError } from "../lib/errors";
import type { FieldError } from "./settings";
import { useSettings } from "./settings";

/**
 * The key's environment variable. Must match `ENV_TYPESAFE_API_KEY` in
 * `src-tauri/src/secrets.rs`.
 */
export const ENV_TYPESAFE_API_KEY = "AEGIS_TYPESAFE_API_KEY";

export type DecisionDraft = {
  readonly model: string;
  readonly baseUrl: string;
  readonly annotateApprovals: boolean;
  /** Write-only. Empty keeps the stored key. */
  readonly apiKey: string;
};

export type DecisionState = {
  /** `null` until the form is filled from saved settings. */
  readonly draft: DecisionDraft | null;
  /** The saved values the draft was filled from. */
  readonly baseline: DecisionDraft | null;
  readonly busy: boolean;
  readonly probe: ProviderProbe | null;
  readonly probing: boolean;
  readonly fieldError: FieldError | null;
  readonly error: IpcError | null;

  /** Fills the form from saved values, unless it is being edited. */
  sync: (saved: MaskedDecision, force?: boolean) => void;
  edit: (patch: Partial<DecisionDraft>) => void;
  save: () => Promise<boolean>;
  clearKey: () => Promise<void>;
  runProbe: () => Promise<void>;
};

function draftOf(saved: MaskedDecision): DecisionDraft {
  return {
    model: saved.model,
    baseUrl: saved.base_url,
    annotateApprovals: saved.annotate_approvals,
    apiKey: "",
  };
}

export const useDecision = create<DecisionState>((set, get) => {
  /** Runs a settings command and hands the result to the roster store. */
  const guard = async (
    command: string,
    run: () => ReturnType<typeof settingsSetDecision>,
  ): Promise<boolean> => {
    set({ busy: true, error: null, fieldError: null });
    try {
      const settings = await run();
      useSettings.getState().applyChanged(settings);
      const saved = draftOf(settings.decision);
      set({ draft: saved, baseline: saved, probe: null });
      return true;
    } catch (cause) {
      const error = toIpcError(cause, command);
      if (error.field !== null) {
        set({ fieldError: { field: error.field, message: error.message } });
      } else {
        set({ error });
      }
      return false;
    } finally {
      set({ busy: false });
    }
  };

  return {
    draft: null,
    baseline: null,
    busy: false,
    probe: null,
    probing: false,
    fieldError: null,
    error: null,

    sync: (saved, force = false) => {
      const { draft, baseline } = get();
      // An event arriving mid-typing must not take the text away.
      const untouched =
        draft === null ||
        (baseline !== null &&
          draft.apiKey.length === 0 &&
          draft.model === baseline.model &&
          draft.baseUrl === baseline.baseUrl &&
          draft.annotateApprovals === baseline.annotateApprovals);
      const next = draftOf(saved);
      set(force || untouched ? { draft: next, baseline: next } : { baseline: next });
    },

    edit: (patch) => {
      const { draft } = get();
      if (draft !== null) {
        set({ draft: { ...draft, ...patch } });
      }
    },

    save: async () => {
      const { draft } = get();
      if (draft === null) {
        return false;
      }
      return guard("settings_set_decision", () => settingsSetDecision(draft));
    },

    clearKey: async () => {
      await guard("settings_clear_decision_key", settingsClearDecisionKey);
    },

    runProbe: async () => {
      set({ probing: true, error: null });
      try {
        set({ probe: await settingsProbeDecision() });
      } catch (cause) {
        set({ error: toIpcError(cause, "settings_probe_decision") });
      } finally {
        set({ probing: false });
      }
    },
  };
});
