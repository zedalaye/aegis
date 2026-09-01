/**
 * Connectors: tools that live in other processes (PLAN 7.3, Phase 18).
 *
 * A section of Settings under Identities, because it is the same question from
 * the other side: an identity is who may call a tool, and a connector is where
 * a tool comes from when this build did not write it.
 *
 * Each row says the four things that decide whether you would leave it
 * installed: what program it runs, whether it is up, what it offers, and — when
 * it is not up — what it said on the way down. The last of those is the one a
 * panel usually withholds, and it is the only thing that makes a mistyped
 * package name fixable: `npx` says what went wrong on stderr and nowhere else.
 *
 * Nothing here is remembered between launches except the record. Whether a
 * process is running is measured, and a row drawn as connected after its server
 * has quietly gone is the one wrong thing this panel could show.
 */

import type { ConnectorView } from "../../ipc/bindings";
import { useConnectors } from "../../state/connectors";

import ConnectorForm from "./ConnectorForm";

/** How each state reads on a row. */
const STATE_LABEL: Record<ConnectorView["state"], string> = {
  off: "not started",
  starting: "starting…",
  ready: "connected",
  failed: "not running",
};

/** One connector. */
function Row({ view }: { readonly view: ConnectorView }) {
  const busy = useConnectors((s) => s.busy);
  const startEdit = useConnectors((s) => s.startEdit);
  const remove = useConnectors((s) => s.remove);
  const setEnabled = useConnectors((s) => s.setEnabled);
  const reconnect = useConnectors((s) => s.reconnect);

  const { connector } = view;
  const line = [connector.command, ...connector.args].join(" ");

  return (
    <li className={`connector connector--${view.state}`}>
      <div className="connector__head">
        <span className="connector__name">{connector.name}</span>
        <code className="connector__id">{connector.id}</code>
        <span className={`connector__badge connector__badge--${view.state}`}>
          {STATE_LABEL[view.state]}
        </span>
        <span className="connector__actions">
          {view.state === "failed" || view.state === "off" ? (
            <button
              type="button"
              className="link"
              // Said on the control because nothing retries by itself: a server
              // that dies four seconds after every start would otherwise hide a
              // broken configuration behind a respawn loop.
              title="Starts it again. Nothing retries on its own."
              onClick={() => void reconnect(connector.id)}
              disabled={busy}
            >
              Reconnect
            </button>
          ) : null}
          <button
            type="button"
            className="link"
            onClick={() => void setEnabled(connector.id, !connector.enabled)}
            disabled={busy}
          >
            {connector.enabled ? "Disable" : "Enable"}
          </button>
          <button
            type="button"
            className="link"
            onClick={() => startEdit(connector.id)}
            disabled={busy}
          >
            Edit
          </button>
          <button
            type="button"
            className="link"
            title="Stops the program and forgets it. Identities that were granted its tools keep those names; they simply stop resolving to anything."
            onClick={() => void remove(connector.id)}
            disabled={busy}
          >
            Remove
          </button>
        </span>
      </div>

      <p className="connector__command">
        <code>{line}</code>
      </p>

      {view.server === null ? null : (
        <p className="connector__server">
          {view.server}
          {view.protocol === null ? null : ` · MCP ${view.protocol}`}
        </p>
      )}

      {view.tools.length === 0 ? (
        <p className="connector__tools connector__tools--none">
          No tools right now.
        </p>
      ) : (
        <ul className="connector__tools" aria-label="Tools">
          {view.tools.map((tool) => (
            <li key={tool.full_name} className="connector__tool">
              <code>{tool.full_name}</code>
              <span className="connector__toolnote">{tool.description}</span>
              {tool.read_only_hint ? (
                // Attributed, not asserted. The server is describing its own
                // gate, so the sentence says whose claim it is — and it changes
                // nothing: every connector call is put to you.
                <span className="connector__hint">
                  the server calls this read-only
                </span>
              ) : null}
            </li>
          ))}
        </ul>
      )}

      {view.missing_env.length === 0 ? null : (
        <p className="connector__problem">
          Not in this application&rsquo;s environment:{" "}
          {view.missing_env.map((name) => (
            <code key={name}>{name}</code>
          ))}
          . Export it before launching Aegis — the value is read from here, not
          stored.
        </p>
      )}

      {view.error === null ? null : (
        <p className="connector__problem">{view.error}</p>
      )}

      {view.log.length === 0 ? null : (
        <details className="connector__log">
          <summary>What it printed</summary>
          <pre>{view.log.join("\n")}</pre>
        </details>
      )}
    </li>
  );
}

export default function ConnectorList() {
  const connectors = useConnectors((s) => s.connectors);
  const status = useConnectors((s) => s.status);
  const busy = useConnectors((s) => s.busy);
  const editing = useConnectors((s) => s.editing);
  const startNew = useConnectors((s) => s.startNew);

  if (status === "loading" && connectors.length === 0) {
    return <p className="settings__note">Loading connectors…</p>;
  }

  const open =
    editing === null
      ? null
      : (connectors.find((view) => view.connector.id === editing) ?? null);

  return (
    <>
      <p className="settings__note">
        A connector is an MCP server: a program on this machine that Aegis
        starts and asks for a list of tools. Those tools reach the model the
        same way <code>fs_write</code> does — offered only to identities that
        hold them, and put to you before every single call, because what a
        program somebody else wrote does with its arguments is not something
        this runtime can check. Adding one starts a program, which is why only
        you can do it: there is no tool that installs a connector.
      </p>

      {connectors.length === 0 && editing === null ? (
        <p className="settings__note">
          No connectors. Aegis has only its own tools.
        </p>
      ) : (
        <ul className="connector__list">
          {connectors.map((view) => (
            <Row key={view.connector.id} view={view} />
          ))}
        </ul>
      )}

      {editing === null ? (
        <button
          type="button"
          className="button"
          onClick={startNew}
          disabled={busy}
        >
          Add connector
        </button>
      ) : (
        <ConnectorForm key={editing} editing={open} />
      )}
    </>
  );
}
