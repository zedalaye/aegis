/**
 * The settings surface: one panel, a roster of providers.
 *
 * Takes over the work area (not a modal) and works with no project open:
 * provider, identities, skills, connectors, memory and routines are
 * application-level.
 */

import { useEffect } from "react";

import { useConnectors } from "../../state/connectors";
import { useRoutines } from "../../state/routines";
import {
  defaultRow,
  effectiveBaseUrl,
  isConfigured,
  useSettings,
} from "../../state/settings";

import AgentList from "../agents/AgentList";
import ConnectorList from "../connectors/ConnectorList";
import MemoryList from "../memory/MemoryList";
import RoutineList from "../routines/RoutineList";
import SkillList from "../skills/SkillList";
import DecisionForm from "./DecisionForm";
import ProviderForm from "./ProviderForm";
import ProviderList from "./ProviderList";

/**
 * What the default provider does, since the built-in Assistant answers from
 * it; mirrors `ProviderSettings::is_configured` for display only.
 */
function ActiveProvider() {
  const settings = useSettings((s) => s.settings);
  if (settings === null) {
    return null;
  }
  const row = defaultRow(settings);
  if (row === undefined) {
    return null;
  }

  return (
    <p className="settings__lede">
      {isConfigured(row) ? (
        <>
          The default provider sends to{" "}
          <code>{effectiveBaseUrl(row, settings.presets)}</code> as{" "}
          <code>{row.model}</code>.
        </>
      ) : (
        <>
          The default provider is not configured, so sessions that answer from
          it get the built-in scripted provider — enough to walk the approval
          gate and the audit log, but there is no model behind it. Name a base
          URL and a model to use a real one.
        </>
      )}
    </p>
  );
}

export default function SettingsPanel() {
  const closePanel = useSettings((s) => s.closePanel);
  const status = useSettings((s) => s.status);
  const loadRoutines = useRoutines((s) => s.load);
  const loadConnectors = useConnectors((s) => s.load);

  // Re-measured when the panel opens. What is *wrong* with a routine — a skill
  // that was un-granted, a folder that was unplugged, a budget spent, a clock
  // the scheduler paused while this was closed — is measured in the runtime and
  // announced by nothing, so the moment somebody comes to look is the moment to
  // ask again. The list itself keeps up on its own through `routine:updated`.
  useEffect(() => {
    void loadRoutines();
  }, [loadRoutines]);

  // And the connectors, for the same reason and a stronger one: a connector's
  // state is a process, measured on every list, and the panel is where somebody
  // finds out that the one they installed last week has not been up since. The
  // rows keep themselves current afterwards through `connector:updated`.
  useEffect(() => {
    void loadConnectors();
  }, [loadConnectors]);

  return (
    <section className="settings" aria-labelledby="settings-title">
      <header className="settings__header">
        <h1 className="settings__title" id="settings-title">
          Settings
        </h1>
        <button type="button" className="button" onClick={closePanel}>
          Close
        </button>
      </header>

      {status === "loading" && <p className="settings__lede">Loading…</p>}
      <ActiveProvider />

      <h2 className="settings__section">Providers</h2>
      <ProviderList />
      <ProviderForm />

      <h2 className="settings__section">Decision model</h2>
      <DecisionForm />

      <h2 className="settings__section">Identities</h2>
      <AgentList />

      <h2 className="settings__section">Connectors</h2>
      <ConnectorList />

      <h2 className="settings__section">Memory</h2>
      <MemoryList />

      <h2 className="settings__section">Skills</h2>
      <SkillList />

      <h2 className="settings__section">Routines</h2>
      <RoutineList />

      <h2 className="settings__section">Where things are kept</h2>
      <p className="settings__note">
        Each provider&rsquo;s label, base URL and model are written to{" "}
        <code>settings.json</code> beside your projects. Identities go in <code>agents.json</code> next to
        them, which is a file you can read and edit by hand. Memories go in{" "}
        <code>memories.json</code>, beside both. Connectors go in{" "}
        <code>connectors.json</code>, which holds the program and its arguments
        and never a secret — a connector names the environment variables it
        needs, and their values are read from this application&rsquo;s own
        environment when it starts one. Runbooks are
        ordinary markdown in <code>skills/</code>, either beside those files or
        inside a workspace, where they travel with the repository. Routines and
        what they have spent today are in <code>routines.json</code>. Keys are
        not: each goes to this machine's own credential store — Credential
        Manager, Keychain, or a Secret Service — under{" "}
        <code>provider-api-key</code> for the default provider and{" "}
        <code>provider-api-key:&lt;id&gt;</code> for the others, and{" "}
        <code>typesafe-api-key</code> for the decision model, and Aegis has
        no command that can read one back out. Nothing in this
        window is ever given the key; it is attached to the request in the
        runtime, as a header, and it is never written to the audit log.
      </p>
    </section>
  );
}
