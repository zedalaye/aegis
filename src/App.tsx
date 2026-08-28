/**
 * Phase 0 shell: proves the WebView boots and renders.
 *
 * The WebView is presentation only. Sessions, tools, policy and secrets all
 * live in the Rust runtime and arrive through typed `invoke` / `listen`
 * wrappers under `src/ipc/` from Phase 1 onward.
 */
export default function App() {
  return (
    <main className="boot">
      <h1 className="boot__title">Aegis</h1>
      <p className="boot__subtitle">Local desktop AI harness</p>
      <p className="boot__phase">
        Phase 0 &mdash; skeleton. The window is up and the WebView renders.
      </p>
    </main>
  );
}
