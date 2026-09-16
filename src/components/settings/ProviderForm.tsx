/**
 * One provider row (PLAN 7.19): a label, a base URL, a model id, and a key.
 *
 * The key is write-only: empty keeps the stored one, and only its source and a
 * four-character hint are shown. Errors land under the field the runtime names.
 */

import { useEffect } from "react";

import type { AuthKind, MaskedProvider } from "../../ipc/bindings";
import {
  DEFAULT_PROVIDER_ID,
  ENV_API_KEY,
  NEW_ROW,
  presetOf,
  rowOf,
  useSettings,
} from "../../state/settings";

const AUTH_LABELS: Readonly<Record<AuthKind, string>> = {
  api_key: "API key (OpenAI-compatible)",
  gemini: "Gemini (Google AI Studio key)",
  claude_cli: "Claude Code login on this machine",
  codex_cli: "Codex CLI login on this machine",
  grok_cli: "Grok CLI login on this machine",
};

function isCliLogin(kind: AuthKind): boolean {
  return kind === "claude_cli" || kind === "codex_cli" || kind === "grok_cli";
}

/** One labelled input, with the refusal that belongs to it. */
function Field({
  id,
  label,
  hint,
  error,
  ...input
}: {
  readonly id: string;
  readonly label: string;
  readonly hint?: string;
  readonly error: string | null;
} & React.InputHTMLAttributes<HTMLInputElement>) {
  return (
    <div className="field">
      <label className="field__label" htmlFor={id}>
        {label}
      </label>
      <input
        id={id}
        className={`field__input${error === null ? "" : " field__input--bad"}`}
        spellCheck={false}
        autoComplete="off"
        aria-invalid={error !== null}
        aria-describedby={error === null ? undefined : `${id}-error`}
        {...input}
      />
      {error === null ? (
        hint === undefined ? null : <p className="field__hint">{hint}</p>
      ) : (
        <p className="field__error" id={`${id}-error`} role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

/** What Aegis currently has for a row's key, in one sentence. */
function KeyStatus({ row }: { readonly row: MaskedProvider }) {
  const clearKey = useSettings((s) => s.clearKey);
  const busy = useSettings((s) => s.busy);

  if (row.key_source === "keyring") {
    return (
      <p className="provider__note">
        A key is stored in this machine's credential store ({row.key_hint}
        ).{" "}
        <button
          type="button"
          className="link"
          onClick={() => void clearKey()}
          disabled={busy}
        >
          Remove it
        </button>
      </p>
    );
  }

  if (row.key_source === "env") {
    return (
      <p className="provider__note">
        Using the key in <code>{ENV_API_KEY}</code> ({row.key_hint}). Aegis
        does not change the environment it was started in, so this one can only
        be removed by unsetting the variable and restarting.
      </p>
    );
  }

  if (row.key_source === "claude_cli") {
    return (
      <CliStatus
        found={row.key_hint}
        cli="claude"
        file="~/.claude/.credentials.json"
      />
    );
  }

  if (row.key_source === "codex_cli") {
    return (
      <CliStatus found={row.key_hint} cli="codex login" file="~/.codex/auth.json" />
    );
  }

  if (row.key_source === "grok_cli") {
    return (
      <CliStatus found={row.key_hint} cli="grok login" file="~/.grok/auth.json" />
    );
  }

  return (
    <p className="provider__note provider__note--warning">
      There is no key. A configured provider with no key answers every message
      with <code>E_NO_API_KEY</code>.
    </p>
  );
}

function CliStatus({
  found,
  cli,
  file,
}: {
  readonly found: string | null;
  readonly cli: string;
  readonly file: string;
}) {
  if (found !== null) {
    return (
      <p className="provider__note">
        Using the official CLI login on this machine ({found}). Aegis reads{" "}
        <code>{file}</code> and never copies the token into the window.
      </p>
    );
  }

  return (
    <p className="provider__note provider__note--warning">
      No CLI login was found at <code>{file}</code>. Run <code>{cli}</code> once,
      or switch back to an API key.
    </p>
  );
}

/** How this machine stores a key, when it will not store one. */
function KeyStorage({
  available,
  isDefault,
}: {
  readonly available: boolean;
  readonly isDefault: boolean;
}) {
  if (available) {
    return null;
  }

  return (
    <p className="provider__note provider__note--warning">
      This machine has no credential store Aegis can use — headless Linux and a
      locked keychain both look like this. A key saved here would have nowhere
      to go.{" "}
      {isDefault ? (
        <>
          Set <code>{ENV_API_KEY}</code> in the environment and restart instead.
        </>
      ) : (
        <>
          Only the default provider reads <code>{ENV_API_KEY}</code>; this one
          needs a credential store, or a CLI login.
        </>
      )}
    </p>
  );
}

/** The last connection test. */
function ProbeResult() {
  const probe = useSettings((s) => s.probe);
  const probing = useSettings((s) => s.probing);

  if (probing) {
    return <p className="provider__probe">Asking the server…</p>;
  }
  if (probe === null) {
    return null;
  }

  return (
    <p
      className={`provider__probe provider__probe--${probe.ok ? "ok" : "bad"}`}
      role="status"
    >
      {probe.message}
      {probe.latency_ms === null ? null : (
        <span className="provider__latency">{probe.latency_ms} ms</span>
      )}
    </p>
  );
}

export default function ProviderForm() {
  const settings = useSettings((s) => s.settings);
  const selected = useSettings((s) => s.selected);
  const remove = useSettings((s) => s.remove);
  const select = useSettings((s) => s.select);
  const draft = useSettings((s) => s.draft);
  const busy = useSettings((s) => s.busy);
  const probing = useSettings((s) => s.probing);
  const fieldError = useSettings((s) => s.fieldError);
  const edit = useSettings((s) => s.edit);
  const save = useSettings((s) => s.save);
  const runProbe = useSettings((s) => s.runProbe);
  const loadModels = useSettings((s) => s.loadModels);
  const models = useSettings((s) => s.models);
  const modelsLive = useSettings((s) => s.modelsLive);
  const modelsMessage = useSettings((s) => s.modelsMessage);
  const modelsBusy = useSettings((s) => s.modelsBusy);

  useEffect(() => {
    if (settings === null) {
      return;
    }
    const handle = window.setTimeout(() => {
      void loadModels();
    }, 400);
    return () => window.clearTimeout(handle);
  }, [draft.authKind, draft.baseUrl, loadModels, selected, settings]);

  if (settings === null) {
    return <p className="provider__note">Loading settings…</p>;
  }

  const adding = selected === NEW_ROW;
  const row = rowOf(settings, selected);
  if (!adding && row === undefined) {
    return null;
  }
  const isDefault = selected === DEFAULT_PROVIDER_ID;

  const errorFor = (field: string) =>
    fieldError?.field === field ? fieldError.message : null;

  const cliAuth = isCliLogin(draft.authKind);
  const geminiAuth = draft.authKind === "gemini";
  const preset = presetOf(settings.presets, draft.authKind);
  const defaultUrl = preset?.default_base_url ?? "";
  const defaultModel = preset?.default_model ?? "";

  return (
    <form
      className="provider"
      onSubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      <Field
        id="provider-label"
        label="Label"
        value={draft.label}
        maxLength={48}
        placeholder={isDefault ? "Default" : "Local server, Claude for review…"}
        error={errorFor("label")}
        hint="How identities and the session picker name this provider. May be left empty."
        onChange={(event) => edit({ label: event.target.value })}
        disabled={busy}
      />

      <div className="field">
        <label className="field__label" htmlFor="provider-auth">
          Authentication
        </label>
        <select
          id="provider-auth"
          className="field__input"
          value={draft.authKind}
          disabled={busy}
          onChange={(event) => {
            const authKind = event.target.value as AuthKind;
            const next = presetOf(settings.presets, authKind);
            const previous = presetOf(settings.presets, draft.authKind);
            const model =
              draft.model.length === 0 ||
              draft.model === previous?.default_model
                ? (next?.default_model ?? draft.model)
                : draft.model;
            const baseUrl =
              draft.baseUrl.length === 0 ||
              draft.baseUrl === previous?.default_base_url
                ? (next?.default_base_url ?? draft.baseUrl)
                : draft.baseUrl;
            edit({ authKind, model, baseUrl });
          }}
        >
          {settings.presets.map((item) => (
            <option key={item.auth_kind} value={item.auth_kind}>
              {AUTH_LABELS[item.auth_kind]}
            </option>
          ))}
        </select>
        <p className="field__hint">
          {cliAuth
            ? "Reuses a login the official CLI already wrote on this machine. Aegis presents itself as that CLI. That is widely done and may be outside the provider's terms."
            : geminiAuth
              ? "A Google AI Studio key (AIza…), stored in this machine's credential store or AEGIS_API_KEY. Aegis talks to the Generative Language API, not an OpenAI-compatible server."
              : isDefault
                ? "An API key stored in this machine's credential store, or AEGIS_API_KEY."
                : "An API key stored in this machine's credential store. Only the default provider reads AEGIS_API_KEY."}
        </p>
      </div>

      <Field
        id="provider-base-url"
        label="Base URL"
        value={draft.baseUrl}
        placeholder={defaultUrl || "https://api.openai.com/v1"}
        error={errorFor("base URL")}
        hint={
          cliAuth
            ? `Leave empty to use ${defaultUrl || "the CLI's own endpoint"}. Change it to point at a proxy or another host.`
            : geminiAuth
              ? `Leave empty to use ${defaultUrl || "Google's Generative Language API"}. Aegis talks to /v1beta/models/{id}:generateContent, so the URL stops at /v1beta.`
              : "Any OpenAI-compatible server. Aegis appends /chat/completions, so the URL stops at /v1. Over http:// the key crosses the network in clear text — use it only for a server on this machine."
        }
        onChange={(event) => edit({ baseUrl: event.target.value })}
        disabled={busy}
      />

      <div className="field">
        <label className="field__label" htmlFor="provider-model">
          Model
        </label>
        <input
          id="provider-model"
          className={`field__input${errorFor("model") === null ? "" : " field__input--bad"}`}
          list="provider-models"
          spellCheck={false}
          autoComplete="off"
          value={draft.model}
          placeholder={defaultModel || "gpt-4o-mini"}
          aria-invalid={errorFor("model") !== null}
          aria-describedby={
            errorFor("model") === null ? undefined : "provider-model-error"
          }
          onChange={(event) => edit({ model: event.target.value })}
          disabled={busy}
        />
        <datalist id="provider-models">
          {models.map((id) => (
            <option key={id} value={id} />
          ))}
        </datalist>
        {errorFor("model") === null ? (
          <p className="field__hint">
            {modelsBusy
              ? "Asking the provider which models it has…"
              : modelsLive
                ? `${models.length} models from the server. Pick one, or type an id it did not list.`
                : modelsMessage.length === 0
                  ? "Sent with every request, exactly as the server spells it."
                  : `${modelsMessage} A built-in list is offered instead.`}
          </p>
        ) : (
          <p className="field__error" id="provider-model-error" role="alert">
            {errorFor("model")}
          </p>
        )}
        {/*
          The ceiling read from the provider's catalog when these settings were
          saved. Shown only while the field still holds the model it was
          resolved for, so an edited-but-unsaved id never borrows the previous
          model's number. Its absence is meaningful too: no line means the
          catalog did not publish one, and the provider's own default applies.
        */}
        {row !== undefined &&
        row.max_output_tokens !== null &&
        draft.model === row.model ? (
          <p className="field__hint">
            Replies — and files written by <code>fs_write</code>, which are
            emitted as tool-call arguments — are capped at{" "}
            {row.max_output_tokens.toLocaleString()} tokens, from this
            provider&rsquo;s catalog.
          </p>
        ) : null}
      </div>

      {cliAuth ? null : (
        <Field
          id="provider-key"
          label="API key"
          type="password"
          value={draft.apiKey}
          placeholder={
            row === undefined || row.key_source === "none"
              ? geminiAuth
                ? "Paste an AI Studio key (AIza…)"
                : "Paste a key"
              : "Leave empty to keep the current key"
          }
          error={null}
          onChange={(event) => edit({ apiKey: event.target.value })}
          disabled={busy || !settings.keyring_available}
        />
      )}

      {row === undefined ? null : <KeyStatus row={row} />}
      {cliAuth ? null : (
        <KeyStorage
          available={settings.keyring_available}
          isDefault={isDefault}
        />
      )}

      <div className="provider__actions">
        <button type="submit" className="button button--primary" disabled={busy}>
          {adding ? "Add provider" : "Save"}
        </button>
        {adding ? (
          <button
            type="button"
            className="button"
            onClick={() => select(DEFAULT_PROVIDER_ID)}
            disabled={busy}
          >
            Cancel
          </button>
        ) : (
          <button
            type="button"
            className="button"
            onClick={() => void runProbe()}
            disabled={busy || probing}
          >
            Test connection
          </button>
        )}
        <button
          type="button"
          className="button"
          onClick={() => void loadModels()}
          disabled={busy || modelsBusy}
        >
          Refresh models
        </button>
        {adding || isDefault ? null : (
          <button
            type="button"
            className="button button--danger"
            onClick={() => void remove(selected)}
            disabled={busy}
            title="Refused while an identity or a session still answers from it. Its stored key goes with it."
          >
            Delete provider
          </button>
        )}
      </div>

      <p className="field__hint">
        Testing sends one very short message to the endpoint above, using the
        model named here — a few tokens, and the only way to tell a wrong
        address from a wrong key from a model that server does not have.
      </p>

      <ProbeResult />
    </form>
  );
}
