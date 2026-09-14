/**
 * Which model answers this session.
 *
 * Derived, never stored (the provider is chosen per turn): the model reported
 * on `turn:started` while a turn runs, otherwise what settings predict.
 */

import { useSessions } from "../../state/sessions";
import { effectiveBaseUrl, isConfigured, useSettings } from "../../state/settings";

export default function ModelBadge() {
  const answering = useSessions((s) => s.streaming?.model ?? null);
  const settings = useSettings((s) => s.settings);

  // A turn in flight: the runtime already said which model it started with,
  // including `aegis-fake-1` when that is the truth. Showing the scripted
  // provider's own id rather than a friendly label is deliberate — a reply
  // that did not come from a model should be impossible to mistake for one.
  if (answering !== null) {
    return (
      <span className="model" title="The model answering this turn">
        {answering}
      </span>
    );
  }

  if (settings === null) {
    return null;
  }

  // Mirrors `ProviderSettings::is_configured`: a CLI login implies its own
  // endpoint, so an empty base URL is not "no provider".
  if (!isConfigured(settings)) {
    return (
      <span
        className="model model--fake"
        title="No provider is configured, so replies come from the built-in scripted provider. There is no model behind it."
      >
        scripted provider
      </span>
    );
  }

  const url = effectiveBaseUrl(settings);
  return (
    <span className="model" title={`Answers from ${url}`}>
      {settings.model}
    </span>
  );
}
