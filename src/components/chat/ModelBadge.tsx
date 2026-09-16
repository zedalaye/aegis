/**
 * Which model answers this session, and the picker that changes it
 * (PLAN 7.19).
 *
 * Derived, never stored (the provider is chosen per turn): the model reported
 * on `turn:started` while a turn runs, otherwise the resolved binding. Picking
 * writes the session's override; the identity, its allow-list and its
 * memories do not change.
 */

import { useState } from "react";

import type { SessionSummary } from "../../ipc/bindings";
import { resolveBinding, useBinding } from "../../state/binding";
import { useAgents } from "../../state/agents";
import { useSessions } from "../../state/sessions";
import {
  effectiveBaseUrl,
  providerName,
  useSettings,
} from "../../state/settings";

/** The value the provider select uses for "no override". */
const INHERIT = "";

function BindingPicker({
  session,
  onClose,
}: {
  readonly session: SessionSummary;
  readonly onClose: () => void;
}) {
  const settings = useSettings((s) => s.settings);
  const agent = useAgents((s) =>
    s.agents.find((candidate) => candidate.id === session.agent_id),
  );
  const busy = useSessions((s) => s.busy);
  const setBinding = useSessions((s) => s.setBinding);
  const [providerId, setProviderId] = useState(session.provider_id ?? INHERIT);
  const [model, setModel] = useState(session.model ?? "");

  if (settings === null) {
    return null;
  }

  // What the session would get with the model field left empty, so the
  // placeholder says what "empty" means for the chosen row.
  const inherited = resolveBinding(settings, agent, {
    provider_id: providerId === INHERIT ? null : providerId,
    model: null,
  });
  const identityDefault = resolveBinding(settings, agent, null);

  return (
    <form
      className="bindingpicker"
      aria-label="Provider and model for this session"
      onSubmit={(event) => {
        event.preventDefault();
        const trimmed = model.trim();
        void setBinding(
          providerId === INHERIT ? null : providerId,
          trimmed.length === 0 ? null : trimmed,
        ).then((ok) => {
          if (ok) {
            onClose();
          }
        });
      }}
    >
      <label className="field__label" htmlFor="binding-provider">
        Provider
      </label>
      <select
        id="binding-provider"
        className="field__input"
        value={providerId}
        onChange={(event) => setProviderId(event.target.value)}
        disabled={busy}
      >
        <option value={INHERIT}>
          Identity default
          {identityDefault.row === undefined
            ? ""
            : ` — ${providerName(identityDefault.row)}`}
        </option>
        {settings.providers.map((row) => (
          <option key={row.id} value={row.id}>
            {providerName(row)}
          </option>
        ))}
      </select>

      <label className="field__label" htmlFor="binding-model">
        Model
      </label>
      <input
        id="binding-model"
        className="field__input"
        value={model}
        placeholder={inherited.model || "the provider's model"}
        onChange={(event) => setModel(event.target.value)}
        spellCheck={false}
        autoComplete="off"
        disabled={busy}
      />
      <p className="field__hint">
        For this session only, from its next message. The identity is not
        changed.
      </p>

      <div className="bindingpicker__actions">
        <button type="submit" className="button button--primary" disabled={busy}>
          Apply
        </button>
        <button
          type="button"
          className="button"
          disabled={busy}
          onClick={() => {
            void setBinding(null, null).then((ok) => {
              if (ok) {
                onClose();
              }
            });
          }}
        >
          Use identity default
        </button>
        <button type="button" className="button" onClick={onClose}>
          Cancel
        </button>
      </div>
    </form>
  );
}

export default function ModelBadge() {
  const answering = useSessions((s) => s.streaming?.model ?? null);
  const session = useSessions((s) => s.detail?.session ?? null);
  const presets = useSettings((s) => s.settings?.presets ?? []);
  const binding = useBinding(session);
  const [open, setOpen] = useState(false);

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

  if (session === null || binding.row === undefined) {
    return null;
  }

  // The runtime refuses a change while a turn runs.
  const locked = session.state === "running" || session.state === "awaiting_approval";
  const marker = binding.overridden ? " •" : "";
  const title = binding.configured
    ? `Answers from ${providerName(binding.row)} at ${effectiveBaseUrl(binding.row, presets)}${
        binding.overridden ? ", overridden for this session" : ""
      }. Click to change.`
    : "This provider is not configured, so replies come from the built-in scripted provider. There is no model behind it. Click to change.";

  return (
    <span className="bindingbadge">
      <button
        type="button"
        className={`model model--button${binding.configured ? "" : " model--fake"}`}
        title={title}
        aria-expanded={open}
        disabled={locked}
        onClick={() => setOpen((was) => !was)}
      >
        {binding.configured ? binding.model : "scripted provider"}
        {marker}
      </button>
      {open ? (
        <BindingPicker
          key={session.id}
          session={session}
          onClose={() => setOpen(false)}
        />
      ) : null}
    </span>
  );
}
