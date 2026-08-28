/**
 * The application root.
 *
 * Everything the WebView shows is assembled under `AppShell`; this file exists
 * only to name the entry point. The WebView is presentation only — projects,
 * paths, persistence and the window lifecycle are all runtime decisions
 * reached through typed `invoke` wrappers.
 */
import AppShell from "./components/layout/AppShell";

export default function App() {
  return <AppShell />;
}
