/**
 * Phase 1 shell: proves the IPC surface and the window lifecycle.
 *
 * The WebView is presentation only. It owns no window state — hiding and
 * quitting are runtime decisions reached through typed `invoke` wrappers, the
 * same code path the tray uses. Sessions, tools, policy and secrets arrive in
 * later phases.
 */
import { useState } from "react";

import { appQuit, windowHide } from "./ipc/commands";
import { IpcError } from "./lib/errors";

export default function App() {
  const [error, setError] = useState<IpcError | null>(null);

  const run = (action: () => Promise<void>) => () => {
    setError(null);
    action().catch((cause: unknown) => {
      setError(cause instanceof IpcError ? cause : null);
    });
  };

  return (
    <main className="boot">
      <h1 className="boot__title">Aegis</h1>
      <p className="boot__subtitle">Local desktop AI harness</p>

      <div className="boot__actions">
        <button type="button" onClick={run(windowHide)}>
          Hide to tray
        </button>
        <button type="button" onClick={run(appQuit)}>
          Quit Aegis
        </button>
      </div>

      {error ? (
        <p className="boot__error" role="alert">
          {error.code}: {error.message}
        </p>
      ) : null}

      <p className="boot__phase">
        Phase 1 &mdash; errors, state, tray, window lifecycle. Closing the
        window hides it; the tray icon brings it back.
      </p>
    </main>
  );
}
