/**
 * The provider roster (PLAN 7.19): one row per chat provider, the default one
 * first. Picking a row puts it in the form below; adding one opens a blank
 * form. Deleting is on the form, where the row's key is.
 */

import type { MaskedProvider } from "../../ipc/bindings";
import {
  DEFAULT_PROVIDER_ID,
  NEW_ROW,
  PROVIDERS_MAX,
  effectiveBaseUrl,
  isConfigured,
  providerName,
  useSettings,
} from "../../state/settings";

/** Where one row sends, in a line. */
function Summary({ row }: { readonly row: MaskedProvider }) {
  const presets = useSettings((s) => s.settings?.presets ?? []);

  if (!isConfigured(row)) {
    return (
      <p className="agent__role">
        Not configured — sessions that answer from it get the scripted
        provider.
      </p>
    );
  }
  return (
    <p className="agent__role">
      <code>{row.model}</code> at <code>{effectiveBaseUrl(row, presets)}</code>
    </p>
  );
}

export default function ProviderList() {
  const settings = useSettings((s) => s.settings);
  const selected = useSettings((s) => s.selected);
  const busy = useSettings((s) => s.busy);
  const select = useSettings((s) => s.select);

  if (settings === null) {
    return null;
  }

  const full = settings.providers.length >= PROVIDERS_MAX;

  return (
    <>
      <p className="settings__note">
        Each provider is somewhere to send a conversation, with its own key.
        Identities choose one, and a session can switch its own from the chat
        header without changing its identity. The default provider answers for
        the built-in Assistant and cannot be removed.
      </p>

      <ul className="agent__list">
        {settings.providers.map((row) => (
          <li key={row.id} className="agent">
            <div className="agent__head">
              <span className="agent__name">{providerName(row)}</span>
              {row.id === DEFAULT_PROVIDER_ID ? (
                <span
                  className="agent__badge"
                  title="Answers for the built-in Assistant, and for any binding whose provider is no longer on file."
                >
                  default
                </span>
              ) : null}
              <span className="agent__actions">
                {selected === row.id ? (
                  <span className="agent__badge">editing</span>
                ) : (
                  <button
                    type="button"
                    className="link"
                    onClick={() => select(row.id)}
                    disabled={busy}
                  >
                    Edit
                  </button>
                )}
              </span>
            </div>
            <Summary row={row} />
          </li>
        ))}
      </ul>

      {selected === NEW_ROW ? null : (
        <p className="settings__note">
          <button
            type="button"
            className="link"
            onClick={() => select(NEW_ROW)}
            disabled={busy || full}
            title={
              full
                ? `At most ${PROVIDERS_MAX} providers on one machine.`
                : undefined
            }
          >
            Add a provider
          </button>
        </p>
      )}
    </>
  );
}
