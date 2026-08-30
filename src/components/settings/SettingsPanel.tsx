/**
 * The settings surface: one panel, one provider.
 *
 * It takes over the work area rather than floating over it as a modal, for the
 * same reason the approval prompt does not float either — the window is one
 * thing at a time, and a dialog that traps focus to prevent a mistake it does
 * not actually prevent is worth less than a page the user can leave.
 *
 * Reachable with no project open, deliberately: configuring where the model
 * comes from is not something that should require picking a folder first. The
 * identities of Phase 12 sit here for the same reason — an identity is a fact
 * about the application, not about any one project. The skills of Phase 13 sit
 * beside them because the two are halves of one question: a skill is what an
 * identity may run, and an identity is who may run a skill. Half of that list
 * does come from the open project's folder, and it is still here rather than in
 * the rail, so "which `inbox.triage` will run" is answerable in one place.
 */

import { useSettings } from "../../state/settings";

import AgentList from "../agents/AgentList";
import SkillList from "../skills/SkillList";
import ProviderForm from "./ProviderForm";

/**
 * Which provider will answer the next message.
 *
 * Mirrors `ProviderSettings::is_configured` in `store/settings.rs`: a base URL
 * and a model together are what make a real request possible. The runtime is
 * still the one that decides — this is the sentence, not the rule — and
 * "Test connection" is the authoritative answer when there is any doubt.
 */
function ActiveProvider() {
  const settings = useSettings((s) => s.settings);
  if (settings === null) {
    return null;
  }

  const configured =
    settings.base_url.length > 0 && settings.model.length > 0;

  return (
    <p className="settings__lede">
      {configured ? (
        <>
          Messages go to <code>{settings.base_url}</code> as{" "}
          <code>{settings.model}</code>.
        </>
      ) : (
        <>
          No provider is configured, so replies come from the built-in scripted
          provider — enough to walk the approval gate and the audit log, but
          there is no model behind it. Name a base URL and a model to use a real
          one.
        </>
      )}
    </p>
  );
}

export default function SettingsPanel() {
  const closePanel = useSettings((s) => s.closePanel);
  const status = useSettings((s) => s.status);

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

      <h2 className="settings__section">Provider</h2>
      <ProviderForm />

      <h2 className="settings__section">Identities</h2>
      <AgentList />

      <h2 className="settings__section">Skills</h2>
      <SkillList />

      <h2 className="settings__section">Where things are kept</h2>
      <p className="settings__note">
        The base URL and the model are written to <code>settings.json</code>{" "}
        beside your projects. Identities go in <code>agents.json</code> next to
        them, which is a file you can read and edit by hand. Runbooks are
        ordinary markdown in <code>skills/</code>, either beside those files or
        inside a workspace, where they travel with the repository. The key is
        not: it
        goes to this machine's own
        credential store — Credential Manager, Keychain, or a Secret Service —
        and Aegis has no command that can read one back out. Nothing in this
        window is ever given the key; it is attached to the request in the
        runtime, as a header, and it is never written to the audit log.
      </p>
    </section>
  );
}
