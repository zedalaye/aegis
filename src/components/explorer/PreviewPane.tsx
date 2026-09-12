/**
 * One file of the open project, shown and not edited (PLAN 7.15).
 *
 * Preview in, save out. Markdown is drawn as elements by `Markdown`, other text
 * sits in a `<pre>`, an image is fetched as bytes and shown from a blob URL
 * this window made, and anything else is its name, size and type. There is no
 * cursor and no Save: changing a file is the operator's editor, or `fs_write`
 * through the approval dialog, and "Show in folder" is how the first is reached.
 *
 * `world/` is previewed exactly like everything else. That it is preview-only
 * is not a special case here — everything is — and it is said on the file,
 * because the constitution is the one place somebody might expect a pencil.
 */

import { useEffect, useState } from "react";

import type { FilePreview, Zone } from "../../ipc/bindings";
import { workspaceImage, workspaceReveal } from "../../ipc/commands";
import { toIpcError } from "../../lib/errors";
import { formatBytes, formatTimestamp } from "../../lib/format";
import { useExplorer } from "../../state/explorer";
import Markdown from "./Markdown";

/** A word about where the file sits, when there is one worth saying. */
const ZONE_NOTE: Record<Zone, string | null> = {
  plain: null,
  briefs: "A brief: work going in.",
  artefacts: "An artefact: work that came out, written under the approval dialog.",
  cabinet: "Shared files. A session changes these through the approval dialog.",
  world: "The constitution. Delegated work reads it and cannot write it.",
};

/** The folder a workspace-relative path is in. */
function folderOf(path: string): string {
  const at = path.lastIndexOf("/");
  return at === -1 ? "" : path.slice(0, at);
}

/**
 * An image, from bytes the runtime handed over.
 *
 * The blob URL is revoked when the image changes or the pane goes away: one
 * left behind keeps the whole picture alive in memory for as long as the window
 * is open.
 */
function ImagePreview({
  projectId,
  preview,
  mime,
}: {
  readonly projectId: string;
  readonly preview: FilePreview;
  readonly mime: string;
}) {
  const [url, setUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    let created: string | null = null;
    setUrl(null);
    setError(null);

    workspaceImage(projectId, preview.path)
      .then((bytes) => {
        if (cancelled) {
          return;
        }
        created = URL.createObjectURL(new Blob([bytes], { type: mime }));
        setUrl(created);
      })
      .catch((cause: unknown) => {
        if (!cancelled) {
          setError(toIpcError(cause, "workspace_image").message);
        }
      });

    return () => {
      cancelled = true;
      if (created !== null) {
        URL.revokeObjectURL(created);
      }
    };
    // `modified` so an image rewritten since it was opened is fetched again.
  }, [projectId, preview.path, preview.modified, mime]);

  if (error !== null) {
    return <p className="preview__note">{error}</p>;
  }
  if (url === null) {
    return <p className="preview__note">Reading…</p>;
  }
  return (
    <div className="preview__imagewrap">
      <img className="preview__image" src={url} alt={preview.name} />
    </div>
  );
}

export default function PreviewPane({ projectId }: { readonly projectId: string }) {
  const selected = useExplorer((s) => s.selected);
  const preview = useExplorer((s) => s.preview);
  const status = useExplorer((s) => s.previewStatus);
  const previewError = useExplorer((s) => s.previewError);
  const openPath = useExplorer((s) => s.openPath);
  const [source, setSource] = useState(false);
  const [revealError, setRevealError] = useState<string | null>(null);

  // A new file opens rendered, whatever the last one was showing.
  useEffect(() => {
    setSource(false);
    setRevealError(null);
  }, [selected]);

  if (selected === null) {
    return (
      <section className="preview preview--empty" aria-label="Preview">
        <p className="preview__note">
          Choose a file to read it here. Nothing in this window saves a file: to
          change one, use your own editor, or ask in a session and the write goes
          through the approval dialog.
        </p>
      </section>
    );
  }

  if (status === "loading" && preview === null) {
    return (
      <section className="preview" aria-label="Preview">
        <p className="preview__note">Reading {selected}…</p>
      </section>
    );
  }

  if (preview === null) {
    return (
      <section className="preview" aria-label="Preview">
        <h2 className="preview__title">{selected}</h2>
        <p className="preview__note" role="status">
          {previewError?.message ?? "This file cannot be shown."}
        </p>
      </section>
    );
  }

  const body = preview.body;
  const reveal = () => {
    setRevealError(null);
    workspaceReveal(projectId, preview.path).catch((cause: unknown) => {
      setRevealError(toIpcError(cause, "workspace_reveal").message);
    });
  };

  return (
    <section className="preview" aria-label={`Preview of ${preview.path}`}>
      <header className="preview__header">
        <div className="preview__heading">
          <h2 className="preview__title">{preview.name}</h2>
          <p className="preview__facts">
            <span className="preview__path">{preview.path}</span>
            <span>{formatBytes(preview.bytes)}</span>
            {preview.modified === null ? null : (
              <span>changed {formatTimestamp(preview.modified)}</span>
            )}
          </p>
        </div>
        <div className="preview__controls">
          {body.kind === "text" && body.markdown ? (
            <button
              type="button"
              className="button"
              aria-pressed={source}
              onClick={() => setSource((showing) => !showing)}
            >
              Source
            </button>
          ) : null}
          <button
            type="button"
            className="button"
            title="Opens the folder in your file manager, with this file selected"
            onClick={reveal}
          >
            Show in folder
          </button>
        </div>
      </header>

      {ZONE_NOTE[preview.zone] === null ? null : (
        <p className="preview__zone">
          {ZONE_NOTE[preview.zone]} Read-only here.
        </p>
      )}
      {revealError === null ? null : (
        <p className="preview__note" role="status">
          {revealError}
        </p>
      )}

      {body.kind === "text" ? (
        <>
          {body.markdown && !source ? (
            <Markdown
              source={body.text}
              base={folderOf(preview.path)}
              onOpenPath={(candidates) => void openPath(candidates)}
            />
          ) : (
            <pre className="preview__text">{body.text}</pre>
          )}
          {body.truncated ? (
            <p className="preview__note">
              Only the first {formatBytes(body.text.length)} are shown. The whole
              file is {formatBytes(preview.bytes)}; Show in folder opens it
              where your own tools can read the rest.
            </p>
          ) : null}
        </>
      ) : null}

      {body.kind === "image" ? (
        <ImagePreview projectId={projectId} preview={preview} mime={body.mime} />
      ) : null}

      {body.kind === "binary" ? (
        <p className="preview__note">
          {body.mime?.startsWith("image/")
            ? `An image (${body.mime}) too large to show here.`
            : body.mime === null
              ? "Not text, and not an image this window shows."
              : `A ${body.mime} file, which this window does not show.`}{" "}
          Show in folder opens it in your file manager, where its own application
          can.
        </p>
      ) : null}
    </section>
  );
}
