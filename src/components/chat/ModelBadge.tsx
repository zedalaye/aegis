/**
 * Which model answers this session.
 *
 * Derived, never stored. A session has no model of its own: the provider is
 * chosen per turn from settings (`AppState::provider`), so a badge reading
 * from a persisted field would be describing whatever was configured the day
 * the session was created. Keeping it derived is also what leaves the
 * post-MVP seam open — a session that later belongs to an *agent* with its own
 * provider binding changes where this reads from, not what it means
 * (`PLAN.md` § 7.1).
 *
 * Two sources, and the distinction is the point:
 *
 * - While a turn is running, the model the runtime reported on `turn:started`.
 *   That is a fact about the reply arriving on screen.
 * - Otherwise, what settings say will answer the next message. That is a
 *   prediction, and it is the honest thing to show when nothing is in flight.
 *
 * When the two would differ — settings changed mid-session — the running turn
 * wins, because the question a user has while text is streaming is "what is
 * writing this", not "what would I get if I asked again".
 */

import { useSessions } from "../../state/sessions";
import { useSettings } from "../../state/settings";

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

  // Mirrors `ProviderSettings::is_configured`: a base URL and a model together
  // are what make a real request possible.
  if (settings.base_url.length === 0 || settings.model.length === 0) {
    return (
      <span
        className="model model--fake"
        title="No provider is configured, so replies come from the built-in scripted provider. There is no model behind it."
      >
        scripted provider
      </span>
    );
  }

  return (
    <span className="model" title={`Answers from ${settings.base_url}`}>
      {settings.model}
    </span>
  );
}
