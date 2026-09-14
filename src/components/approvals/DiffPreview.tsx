/**
 * The structured detail of an approval: what would actually happen.
 *
 * With no sandbox (PLAN 3.3), this is what the person reads before anything
 * runs, so everything is fields and plain text, never markup.
 *
 * - `fs_write`: path, size, whether it overwrites, and the first 4 KB.
 * - Connector calls: name, the server's description and the model's arguments,
 *   each labelled by source — the runtime cannot say what will happen.
 * - `screen_capture`: display and sizes, no preview (PLAN 5.4).
 */

import type { ApprovalDetail } from "../../ipc/bindings";

/** A size a person can read. */
function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  const units = ["KB", "MB", "GB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(1)} ${units[unit]}`;
}

/** One labelled row. Paths and commands are monospaced and wrap anywhere. */
function Field({
  label,
  children,
  mono = false,
}: {
  readonly label: string;
  readonly children: React.ReactNode;
  readonly mono?: boolean;
}) {
  return (
    <div className="detail__row">
      <dt className="detail__label">{label}</dt>
      <dd className={mono ? "detail__value detail__value--mono" : "detail__value"}>
        {children}
      </dd>
    </div>
  );
}

export default function DiffPreview({
  detail,
}: {
  readonly detail: ApprovalDetail;
}) {
  switch (detail.kind) {
    case "fs_list":
      return (
        <dl className="detail">
          <Field label="Folder" mono>
            {detail.path}
          </Field>
        </dl>
      );

    case "fs_read":
      return (
        <dl className="detail">
          <Field label="File" mono>
            {detail.path}
          </Field>
          <Field label="Size">
            {detail.bytes === null
              ? "unknown — the file may not exist"
              : formatBytes(detail.bytes)}
          </Field>
        </dl>
      );

    case "fs_write":
      return (
        <>
          <dl className="detail">
            <Field label="File" mono>
              {detail.path}
            </Field>
            <Field label="Writing">{formatBytes(detail.bytes)}</Field>
            <Field label="Target">
              {detail.exists
                ? "a file that already exists — this replaces its contents"
                : "a new file"}
            </Field>
          </dl>
          {detail.preview === null ? null : (
            <figure className="preview">
              <figcaption className="preview__caption">
                What would be written
                {detail.preview.length < detail.bytes
                  ? " (first part only)"
                  : ""}
              </figcaption>
              <pre className="preview__body">{detail.preview}</pre>
            </figure>
          )}
          {/*
            The apply of a proposal (PLAN 7.13). The runtime recognised it from
            what the write is — the PROPOSAL.md beside the target, copied byte
            for byte — so this note is a fact, not the model's description of
            its own call.
          */}
          {detail.applies === null ? null : (
            <p className="detail__note">
              This applies the proposal for <code>{detail.applies}</code>: what
              is shown above becomes a runbook in this workspace&apos;s catalog.
              Read it as the runbook you are signing. It is granted to no
              identity by this — that is still a tick in Settings.
            </p>
          )}
        </>
      );

    case "shell":
      return (
        <>
          <dl className="detail">
            <Field label="Command" mono>
              {detail.shell_line}
            </Field>
            <Field label="Program" mono>
              {detail.program}
            </Field>
            {/*
              With an execution host (PLAN 7.12) there are two true answers to
              "where", and showing one of them would be a dialog describing a
              directory the command never sees. The Linux path is the working
              directory the program actually starts in; the Windows path is the
              same folder as the file tools spell it, and as containment
              measured it. Which machine it is stands between them, because
              without that the two paths look like a contradiction.
            */}
            {detail.host === null ? (
              <Field label="Working directory" mono>
                {detail.cwd}
              </Field>
            ) : (
              <>
                <Field label="Runs in">
                  {detail.host.distro} — a WSL distribution on this machine, not
                  Windows
                </Field>
                <Field label="Working directory" mono>
                  {detail.host.cwd}
                </Field>
                <Field label="Same folder on Windows" mono>
                  {detail.cwd}
                </Field>
              </>
            )}
          </dl>
          {detail.args.length === 0 ? null : (
            <ol className="args" aria-label="Arguments">
              {detail.args.map((argument, index) => (
                // Arguments are positional and may repeat, so the index is the
                // identity here — there is nothing else that distinguishes two
                // identical ones.
                // eslint-disable-next-line react/no-array-index-key
                <li key={`${index}-${argument}`} className="args__item">
                  {argument}
                </li>
              ))}
            </ol>
          )}
        </>
      );

    case "screen": {
      // Physical and logical differ on a scaled display: the file would be the
      // physical size, and the screen the user is looking at is the logical
      // one (PLAN 5.1). Both are shown, and neither is called "the size".
      const known = detail.width > 0 && detail.height > 0;
      const scaled =
        known &&
        detail.logical_width > 0 &&
        (detail.logical_width !== detail.width ||
          detail.logical_height !== detail.height);

      return (
        <>
          <dl className="detail">
            <Field label="Display">{detail.display}</Field>
            <Field label="Image">
              {known ? `${detail.width} × ${detail.height} pixels` : "unknown"}
            </Field>
            {scaled ? (
              <Field label="On screen">
                {`${detail.logical_width} × ${detail.logical_height} points`}
              </Field>
            ) : null}
          </dl>
          {/*
            No preview of what would be captured, deliberately. Every other
            approval can show the thing it is about because reading a path or a
            command line costs nothing; taking a picture of the screen to ask
            whether a picture of the screen is allowed would be doing the thing
            being asked about (PLAN 5.4). What the prompt can honestly say is
            which display, how big, and what a capture contains.
          */}
          <p className="detail__note">
            A capture holds everything on that display at the moment you allow
            it — every window, not only Aegis. It is written outside your
            workspace and this build cannot read it back into the conversation.
          </p>
        </>
      );
    }

    case "memory":
      // The whole sentence, not a preview of it. A memory is one sentence by
      // construction, and it is the one mutating call where reading the entire
      // thing costs the user less than reading a summary would.
      return (
        <>
          <dl className="detail">
            <Field label="Kind">{detail.memory_kind}</Field>
            <Field label="Rests on" mono>
              {detail.source ?? "nothing — it would be read as a hypothesis"}
            </Field>
          </dl>
          <figure className="preview">
            <figcaption className="preview__caption">
              What would be remembered
            </figcaption>
            <pre className="preview__body">{detail.text}</pre>
          </figure>
          <p className="detail__note">
            This reaches the top of every later reply this identity gives, in
            this session and in every session after it. Nothing on your machine
            changes. You can read, correct and delete it under Memory in
            Settings; the model cannot delete it.
          </p>
        </>
      );

    case "handoff":
      // Who is about to work on what — which is the decision COS.md gives the
      // human, and the only one this dialog is asking. The constraints and the
      // definition of done are in the brief file named below and in the
      // transcript of each run; reproducing four whole briefs here would be a
      // dialog nobody reads.
      return (
        <>
          <ol className="briefs" aria-label="Briefs">
            {detail.briefs.map((brief) => (
              <li key={`${brief.owner}-${brief.goal}`} className="briefs__item">
                <span className="briefs__owner">{brief.owner}</span>
                <span className="briefs__goal">{brief.goal}</span>
                <span className="briefs__meta">
                  {brief.priority} · wants a {brief.return_format} ·{" "}
                  {brief.inputs === 1 ? "1 input" : `${brief.inputs} inputs`}
                </span>
              </li>
            ))}
          </ol>
          <dl className="detail">
            <Field label="Reviewed by">
              {detail.reviewer ??
                "nobody — what comes back goes straight to this session"}
            </Field>
            <Field label="Filed in" mono>
              {detail.filed_in ??
                "nowhere — this workspace has no briefs/ folder yet"}
            </Field>
          </dl>
          <p className="detail__note">
            Each of these opens a session of its own, under the identity named,
            and runs at the same time as the others. They work with the tools
            that identity holds — not with yours, and not with anything you have
            allowed for this session: whatever they want to write or run asks
            you again, in their own sessions. What comes back here is a status,
            an artefact list and any open questions, never their conversations.
          </p>
        </>
      );

    case "connector":
      return (
        <>
          <dl className="detail">
            <Field label="Connector">
              {detail.connector_name} (<code>{detail.connector}</code>)
            </Field>
            <Field label="Tool" mono>
              {detail.tool}
            </Field>
            <Field label="The server says">{detail.description}</Field>
          </dl>
          <figure className="preview">
            <figcaption className="preview__caption">
              What the model wrote as arguments
            </figcaption>
            <pre className="preview__body">{detail.arguments}</pre>
          </figure>
          <p className="detail__note">
            This runs inside a program Aegis did not write. The arguments above
            are sent to it exactly as they are — nothing here resolves a path or
            checks what any of them mean, because the tool runs somewhere else
            and this runtime has never seen its schema.
            {detail.read_only_hint
              ? " The server describes this tool as read-only. That is the server's own claim about itself, so it changes nothing about being asked."
              : ""}{" "}
            Allowing it for the session covers this one tool and nothing else
            the connector offers — including anything it adds later.
          </p>
        </>
      );
  }
}
