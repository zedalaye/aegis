/**
 * Where this project's commands run, in the rail (PLAN 7.12).
 *
 * One control: a list of this computer and every WSL distribution installed on
 * it. Picking one means `shell_exec` runs the program inside that distribution
 * — on that distribution's PATH, as that distribution's own user — instead of
 * spawning it on Windows. Nothing else changes: `fs_read`, `fs_write` and
 * `fs_list` still work on the folder through Windows, and the approval gate is
 * the same gate.
 *
 * It sits on the project's side of the rail, above the shared files, because it
 * is the most consequential fact about the folder here — it decides which
 * machine's toolchain the work is done with — and because a person setting up a
 * project wants it before they start rather than after the first `pnpm` fails.
 *
 * When the folder itself lives in a distribution, that row says so. It is a
 * label, not a decision: PLAN 7.12 forbids flipping the host from a `\wsl$\`
 * path, because picking a folder is not consent — and because the inference
 * would only catch one of the two spellings, `C:\work` being just as reachable
 * from the distribution at `/mnt/c/work`. Marking the row makes the likely
 * answer findable and still leaves the choosing to the person.
 *
 * The panel draws nothing at all on a machine with no distributions, unless the
 * project already names one. A picker with a single row is not a choice, and on
 * macOS, on Linux, and on a Windows box without WSL there genuinely is only one
 * place a command can go — while a project carrying a host that this machine
 * cannot offer is exactly the case somebody has to be able to see and undo.
 */

import { useEffect } from "react";

import Section from "../layout/Section";
import { useHosts } from "../../state/hosts";
import { useProjects } from "../../state/projects";

export default function ExecHost() {
  const project = useProjects((s) => s.detail?.project ?? null);
  const options = useHosts((s) => s.options);
  const loaded = useHosts((s) => s.loaded);
  const busy = useHosts((s) => s.busy);
  const load = useHosts((s) => s.load);
  const choose = useHosts((s) => s.set);

  // Re-asked whenever another project is opened, which is the cheapest honest
  // approximation of "when somebody might look": distributions are installed
  // in a terminal, and this window is never told.
  useEffect(() => {
    void load();
  }, [load, project?.id]);

  if (project === null) {
    return null;
  }

  const chosen = project.exec_host?.distro ?? null;
  const installed = options.flatMap((option) =>
    option.kind === "wsl" ? [option.distro] : [],
  );

  // The distribution this folder is *in*, when it is in one — measured from the
  // path, never acted on. Only worth naming while it is not already the host:
  // once it is chosen, the note below says where commands go, and saying it
  // twice would read as two different facts.
  const inside =
    project.workspace_distro !== null &&
    project.workspace_distro !== chosen &&
    installed.includes(project.workspace_distro)
      ? project.workspace_distro
      : null;

  // Nothing to offer, and nothing to undo.
  if (installed.length === 0 && chosen === null) {
    return null;
  }

  // A project can name a distribution that has since been uninstalled, and it
  // stays in the list so the control shows what the project actually says —
  // otherwise the picker reads as "this computer" while every command fails.
  // Only called *missing* once the list has actually been read back.
  const absent = chosen !== null && !installed.includes(chosen);
  const missing = absent && loaded;
  const rows = absent ? [...installed, chosen] : installed;

  return (
    <Section
      id="exec-host"
      title="Commands run in"
      className="host"
      badge={
        chosen === null ? null : (
          <span
            className={`rail__count${missing ? " rail__count--attention" : ""}`}
          >
            {missing ? "missing" : chosen}
          </span>
        )
      }
    >
      <label className="host__label" htmlFor="exec-host-select">
        Execution host
      </label>
      <select
        id="exec-host-select"
        className="host__select"
        value={chosen ?? ""}
        disabled={busy}
        onChange={(event) => {
          const distro = event.target.value;
          void choose(
            project.id,
            distro === "" ? null : { kind: "wsl", distro },
          );
        }}
      >
        <option value="">This computer</option>
        {rows.map((distro) => (
          <option key={distro} value={distro}>
            {distro === chosen && missing
              ? `${distro} — not installed`
              : distro === inside
                ? `${distro} (WSL) — this folder is in it`
                : `${distro} (WSL)`}
          </option>
        ))}
      </select>

      <p className="host__note">
        {chosen === null ? (
          <>
            <code>shell_exec</code> runs the program on this computer. Pick a
            distribution if this project's toolchain lives in one — the files
            are read and written the same way either way.
          </>
        ) : (
          <>
            <code>shell_exec</code> runs the program inside <code>{chosen}</code>
            , on its PATH and as its own user, in the Linux path that matches
            this folder. <code>fs_read</code>, <code>fs_write</code> and{" "}
            <code>fs_list</code> are unchanged.
          </>
        )}
      </p>

      {inside === null ? null : (
        <p className="host__note" role="status">
          This folder is inside <code>{inside}</code>. Commands still run on
          this computer until you pick it — opening a folder is not the same as
          choosing where its commands go.
        </p>
      )}

      {missing ? (
        <p className="host__note host__note--warn" role="status">
          <code>{chosen}</code> is not installed on this machine any more, so
          every command will be refused until this is changed.
        </p>
      ) : null}
    </Section>
  );
}
