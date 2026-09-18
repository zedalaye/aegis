/**
 * The decision model (PLAN 7.18): a TypeSafe key, a model, an origin, and
 * whether approval dialogs are annotated.
 *
 * Separate from the provider form on purpose: Jev never writes a reply, and
 * saving here sends no provider field.
 */

import { useEffect } from "react";

import type { MaskedDecision } from "../../ipc/bindings";
import { ENV_TYPESAFE_API_KEY, useDecision } from "../../state/decision";
import { useSettings } from "../../state/settings";

/** What Aegis has for the TypeSafe key, in one sentence. */
function KeyStatus({ saved }: { readonly saved: MaskedDecision }) {
  const clearKey = useDecision((s) => s.clearKey);
  const busy = useDecision((s) => s.busy);

  if (saved.key_source === "keyring") {
    return (
      <p className="provider__note">
        A TypeSafe key is stored in this machine&rsquo;s credential store (
        {saved.key_hint}).{" "}
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
  if (saved.key_source === "env") {
    return (
      <p className="provider__note">
        Using the key in <code>{ENV_TYPESAFE_API_KEY}</code> ({saved.key_hint}).
      </p>
    );
  }
  return (
    <p className="provider__note">
      No TypeSafe key. That is fine: the decision model is optional. Without a
      key, dialogs open unannotated and the <code>jev_*</code> tools are not
      offered to any identity.
    </p>
  );
}

export default function DecisionForm() {
  const settings = useSettings((s) => s.settings);
  const draft = useDecision((s) => s.draft);
  const sync = useDecision((s) => s.sync);
  const edit = useDecision((s) => s.edit);
  const save = useDecision((s) => s.save);
  const runProbe = useDecision((s) => s.runProbe);
  const busy = useDecision((s) => s.busy);
  const probe = useDecision((s) => s.probe);
  const probing = useDecision((s) => s.probing);
  const fieldError = useDecision((s) => s.fieldError);
  const error = useDecision((s) => s.error);

  const saved = settings?.decision;
  useEffect(() => {
    if (saved !== undefined) {
      sync(saved);
    }
  }, [saved, sync]);

  if (settings === null || saved === undefined || draft === null) {
    return null;
  }

  const errorFor = (field: string) =>
    fieldError?.field === field ? fieldError.message : null;
  const modelError = errorFor("model");
  const urlError = errorFor("decision base URL");

  return (
    <form
      className="provider"
      onSubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      <p className="field__hint">
        TypeSafe Jev answers typed questions with probabilities. It does not
        write replies and never answers a turn — the chat provider above still
        does. Aegis uses it to annotate approval dialogs, and identities you
        grant <code>jev_eval</code> or <code>jev_ask</code> can use it too.
        Every such call is still asked about, and no answer ever allows or
        denies one.
      </p>

      <div className="field">
        <label className="field__label" htmlFor="decision-key">
          TypeSafe API key
        </label>
        <input
          id="decision-key"
          className="field__input"
          type="password"
          spellCheck={false}
          autoComplete="off"
          value={draft.apiKey}
          placeholder={
            saved.key_source === "none"
              ? "Paste a key"
              : "Leave empty to keep the current key"
          }
          onChange={(event) => edit({ apiKey: event.target.value })}
          disabled={busy || !settings.keyring_available}
        />
      </div>
      <KeyStatus saved={saved} />

      <div className="field">
        <label className="field__label" htmlFor="decision-model">
          Model
        </label>
        <input
          id="decision-model"
          className={`field__input${modelError === null ? "" : " field__input--bad"}`}
          spellCheck={false}
          autoComplete="off"
          value={draft.model}
          placeholder={saved.default_model}
          onChange={(event) => edit({ model: event.target.value })}
          disabled={busy}
        />
        {modelError === null ? (
          <p className="field__hint">Leave empty for {saved.default_model}.</p>
        ) : (
          <p className="field__error" role="alert">
            {modelError}
          </p>
        )}
      </div>

      <div className="field">
        <label className="field__label" htmlFor="decision-url">
          Base URL
        </label>
        <input
          id="decision-url"
          className={`field__input${urlError === null ? "" : " field__input--bad"}`}
          spellCheck={false}
          autoComplete="off"
          value={draft.baseUrl}
          placeholder={saved.default_base_url}
          onChange={(event) => edit({ baseUrl: event.target.value })}
          disabled={busy}
        />
        {urlError === null ? (
          <p className="field__hint">
            The origin only; Aegis appends <code>/v1/systemone</code>. Leave
            empty for {saved.default_base_url}.
          </p>
        ) : (
          <p className="field__error" role="alert">
            {urlError}
          </p>
        )}
      </div>

      <label className="connectorform__enabled">
        <input
          type="checkbox"
          checked={draft.annotateApprovals}
          onChange={(event) =>
            edit({ annotateApprovals: event.target.checked })
          }
          disabled={busy}
        />
        Annotate approval dialogs
      </label>
      <p className="field__hint">
        With a key, each dialog&rsquo;s summary and preview are sent to TypeSafe
        and the answer is shown as advice under the reason. Off keeps the key
        and sends nothing for dialogs.
      </p>

      {error === null ? null : (
        <p className="provider__note provider__note--warning" role="alert">
          {error.message}
        </p>
      )}

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

      {probing ? (
        <p className="provider__probe">Asking TypeSafe…</p>
      ) : probe === null ? null : (
        <p
          className={`provider__probe provider__probe--${probe.ok ? "ok" : "bad"}`}
          role="status"
        >
          {probe.message}
          {probe.latency_ms === null ? null : (
            <span className="provider__latency">{probe.latency_ms} ms</span>
          )}
        </p>
      )}
    </form>
  );
}
