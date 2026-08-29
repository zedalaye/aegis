/**
 * Where the model comes from: a base URL, a model id, and a key.
 *
 * The key field is write-only and starts empty every time. Leaving it empty
 * keeps whatever is stored — changing a model is not a reason to retype a
 * credential — and there is no way to read a key back out, because the WebView
 * is never given one. What it can show is where the key came from and four
 * characters of it, which is enough to recognize *which* key is installed and
 * not enough to use it.
 *
 * A refused value lands under the input it is about rather than in a banner,
 * because the runtime says which field it was talking about.
 */

import type { MaskedSettings } from "../../ipc/bindings";
import { ENV_API_KEY, useSettings } from "../../state/settings";

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

/** What Aegis currently has for a key, in one sentence. */
function KeyStatus({ settings }: { readonly settings: MaskedSettings }) {
  const clearKey = useSettings((s) => s.clearKey);
  const busy = useSettings((s) => s.busy);

  if (settings.key_source === "keyring") {
    return (
      <p className="provider__note">
        A key is stored in this machine's credential store ({settings.key_hint}
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

  if (settings.key_source === "env") {
    return (
      <p className="provider__note">
        Using the key in <code>{ENV_API_KEY}</code> ({settings.key_hint}). Aegis
        does not change the environment it was started in, so this one can only
        be removed by unsetting the variable and restarting.
      </p>
    );
  }

  return (
    <p className="provider__note provider__note--warning">
      There is no key. A configured provider with no key answers every message
      with <code>E_NO_API_KEY</code>.
    </p>
  );
}

/** How this machine stores a key, when it will not store one. */
function KeyStorage({ settings }: { readonly settings: MaskedSettings }) {
  if (settings.keyring_available) {
    return null;
  }

  return (
    <p className="provider__note provider__note--warning">
      This machine has no credential store Aegis can use — headless Linux and a
      locked keychain both look like this. A key saved here would have nowhere
      to go, so set <code>{ENV_API_KEY}</code> in the environment and restart
      instead.
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
  const draft = useSettings((s) => s.draft);
  const busy = useSettings((s) => s.busy);
  const probing = useSettings((s) => s.probing);
  const fieldError = useSettings((s) => s.fieldError);
  const edit = useSettings((s) => s.edit);
  const save = useSettings((s) => s.save);
  const runProbe = useSettings((s) => s.runProbe);

  if (settings === null) {
    return <p className="provider__note">Loading settings…</p>;
  }

  const errorFor = (field: string) =>
    fieldError?.field === field ? fieldError.message : null;

  return (
    <form
      className="provider"
      onSubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      <Field
        id="provider-base-url"
        label="Base URL"
        value={draft.baseUrl}
        placeholder="https://api.openai.com/v1"
        error={errorFor("base URL")}
        hint="Any OpenAI-compatible server. Aegis appends /chat/completions, so the URL stops at /v1. Over http:// the key crosses the network in clear text — use it only for a server on this machine."
        onChange={(event) => edit({ baseUrl: event.target.value })}
        disabled={busy}
      />

      <Field
        id="provider-model"
        label="Model"
        value={draft.model}
        placeholder="gpt-4o-mini"
        error={errorFor("model")}
        hint="Sent with every request, exactly as the server spells it."
        onChange={(event) => edit({ model: event.target.value })}
        disabled={busy}
      />

      <Field
        id="provider-key"
        label="API key"
        type="password"
        value={draft.apiKey}
        placeholder={
          settings.key_source === "none"
            ? "Paste a key"
            : "Leave empty to keep the current key"
        }
        error={null}
        onChange={(event) => edit({ apiKey: event.target.value })}
        disabled={busy || !settings.keyring_available}
      />

      <KeyStatus settings={settings} />
      <KeyStorage settings={settings} />

      <div className="provider__actions">
        <button type="submit" className="button button--primary" disabled={busy}>
          Save
        </button>
        <button
          type="button"
          className="button"
          onClick={() => void runProbe()}
          disabled={busy || probing}
        >
          Test connection
        </button>
      </div>

      <ProbeResult />
    </form>
  );
}
