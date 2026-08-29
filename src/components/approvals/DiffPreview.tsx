/**
 * The structured detail of an approval: what would actually happen.
 *
 * This is the part that matters. The MVP has no sandbox (PLAN 3.3) — the
 * boundary is that a person reads the exact path, program, arguments and
 * working directory before anything mutating runs. So everything here is drawn
 * as fields and plain text, never as a JSON blob and never as markup: the
 * content is the model's output, and a preview that rendered it would put
 * whatever it said one escaping bug away from the DOM.
 *
 * "Diff" is the name PLAN gives this pane; for `fs_write` the runtime supplies
 * the first 4 KB of the pending content, and whether the file already exists.
 * A real before/after diff needs the old text too, which is a Phase 10 nicety —
 * what a user needs before allowing a write is the path, the size, whether it
 * overwrites, and a look at what is going in.
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
            <Field label="Working directory" mono>
              {detail.cwd}
            </Field>
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

    case "screen":
      return (
        <dl className="detail">
          <Field label="Display">{detail.display}</Field>
          <Field label="Size">
            {detail.width === 0 || detail.height === 0
              ? "unknown"
              : `${detail.width} × ${detail.height}`}
          </Field>
        </dl>
      );
  }
}
