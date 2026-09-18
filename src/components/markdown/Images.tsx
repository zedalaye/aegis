/**
 * Local images, drawn without widening `img-src` (PLAN 7.15, 7.20).
 *
 * A workspace image arrives as runtime bytes behind a `blob:` URL; a capture
 * or an attachment through the scoped `asset:` protocol (PLAN 5.4). Nothing
 * here fetches a remote address.
 */

import { convertFileSrc } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import type { ReactNode } from "react";

import { workspaceImage } from "../../ipc/commands";
import { toIpcError } from "../../lib/errors";

/** Where a workspace image stands. */
export type ImageLoad =
  | { readonly state: "loading" }
  | { readonly state: "ready"; readonly url: string }
  | { readonly state: "error"; readonly message: string };

/**
 * One workspace image as a blob URL, revoked on change or unmount. `stamp`
 * refetches a file rewritten since it was drawn.
 */
export function useWorkspaceImage(
  projectId: string,
  path: string,
  mime?: string,
  stamp?: string | null,
): ImageLoad {
  const [load, setLoad] = useState<ImageLoad>({ state: "loading" });

  useEffect(() => {
    let cancelled = false;
    let created: string | null = null;
    setLoad({ state: "loading" });

    workspaceImage(projectId, path)
      .then((bytes) => {
        if (cancelled) {
          return;
        }
        // Without a type the WebView sniffs the bytes, which the runtime
        // already checked are an image.
        created = URL.createObjectURL(
          new Blob([bytes], mime === undefined ? {} : { type: mime }),
        );
        setLoad({ state: "ready", url: created });
      })
      .catch((cause: unknown) => {
        if (!cancelled) {
          setLoad({
            state: "error",
            message: toIpcError(cause, "workspace_image").message,
          });
        }
      });

    return () => {
      cancelled = true;
      if (created !== null) {
        URL.revokeObjectURL(created);
      }
    };
  }, [projectId, path, mime, stamp]);

  return load;
}

/** A workspace image inside text; `fallback` when it cannot be drawn. */
export function WorkspaceImage({
  projectId,
  path,
  alt,
  fallback,
}: {
  readonly projectId: string;
  readonly path: string;
  readonly alt: string;
  readonly fallback: ReactNode;
}) {
  const load = useWorkspaceImage(projectId, path);
  if (load.state === "error") {
    return fallback;
  }
  if (load.state === "loading") {
    return <span className="md__image">[{alt.length > 0 ? alt : path}…]</span>;
  }
  return <img className="md__img" src={load.url} alt={alt} title={path} />;
}

/**
 * A file under the app's own capture or attachment directories, through the
 * scoped `asset:` protocol. A path outside that scope simply fails to load.
 */
export function AssetImage({
  path,
  alt,
  fallback,
  className = "md__img",
}: {
  readonly path: string;
  readonly alt: string;
  readonly fallback: ReactNode;
  readonly className?: string;
}) {
  const [broken, setBroken] = useState(false);
  useEffect(() => setBroken(false), [path]);
  if (broken) {
    return fallback;
  }
  return (
    <img
      className={className}
      src={convertFileSrc(path)}
      alt={alt}
      title={path}
      onError={() => setBroken(true)}
    />
  );
}
