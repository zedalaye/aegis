/**
 * Markdown, drawn as elements (PLAN 7.15, 7.20).
 *
 * Maps `lib/markdown.ts` nodes to elements. Never `dangerouslySetInnerHTML`:
 * every string is a text node. Nothing here is an `<a href>` — the window
 * never navigates. Relative links and path-like code spans open in the
 * (contained) preview; web links open the OS browser when the host passes
 * `onOpenUrl`, and are shown with their address either way.
 */

import { createContext, createElement, useContext, useMemo } from "react";
import type { ReactNode } from "react";

import {
  isExternal,
  looksLikePath,
  parseMarkdown,
  plain,
  resolveRelative,
  webUrl,
} from "../../lib/markdown";
import type { Block, Inline } from "../../lib/markdown";

/** An image the document embeds, as the host is asked to draw it. */
export type EmbeddedImage = {
  readonly alt: string;
  /** The destination as written. */
  readonly src: string;
  /** The workspace path it resolves to, when it is relative. */
  readonly target: string | null;
};

type Props = {
  readonly source: string;
  /** The folder the text is in, workspace-relative, so its links resolve. */
  readonly base: string;
  /**
   * Opens a workspace file in the preview. Several candidates, tried in order:
   * a brief names its inputs from the workspace root, a README names its
   * neighbours from its own folder, and a code span does not say which.
   */
  readonly onOpenPath: (candidates: readonly string[]) => void;
  /** Opens an `http`/`https` address in the OS browser. Absent: shown only. */
  readonly onOpenUrl?: (url: string) => void;
  /** Draws an embedded image; `null` keeps the default control. */
  readonly drawImage?: (image: EmbeddedImage) => ReactNode;
  /** Drawn after the last block, inside it — the streaming caret. */
  readonly trailer?: ReactNode;
  readonly className?: string;
};

type Context = Pick<Props, "base" | "onOpenPath" | "onOpenUrl" | "drawImage">;

const Hooks = createContext<Context>({ base: "", onOpenPath: () => undefined });

export default function Markdown({
  source,
  base,
  onOpenPath,
  onOpenUrl,
  drawImage,
  trailer,
  className,
}: Props) {
  const blocks = useMemo(() => parseMarkdown(source), [source]);
  const hooks = useMemo<Context>(
    () => ({
      base,
      onOpenPath,
      ...(onOpenUrl === undefined ? {} : { onOpenUrl }),
      ...(drawImage === undefined ? {} : { drawImage }),
    }),
    [base, onOpenPath, onOpenUrl, drawImage],
  );

  return (
    <Hooks.Provider value={hooks}>
      <div className={className === undefined ? "md" : `md ${className}`}>
        {blocks.map((block, index) => (
          <BlockNode key={index} block={block} />
        ))}
        {trailer}
      </div>
    </Hooks.Provider>
  );
}

function Blocks({ blocks }: { readonly blocks: readonly Block[] }): ReactNode {
  return blocks.map((child, index) => <BlockNode key={index} block={child} />);
}

function BlockNode({ block }: { readonly block: Block }) {
  switch (block.kind) {
    case "heading":
      return createElement(
        `h${block.level}`,
        { className: `md__h md__h${block.level}` },
        <Runs runs={block.content} />,
      );
    case "paragraph":
      return (
        <p className="md__p">
          <Runs runs={block.content} />
        </p>
      );
    case "code":
      return (
        <pre className="md__pre" data-info={block.info || undefined}>
          <code>{block.text}</code>
        </pre>
      );
    case "quote":
      return (
        <blockquote className="md__quote">
          <Blocks blocks={block.children} />
        </blockquote>
      );
    case "rule":
      return <hr className="md__rule" />;
    case "list": {
      const items = block.items.map((item, index) => {
        // A tight item is one paragraph; drawing it without the paragraph's
        // margins is what keeps a status board's bullets from double-spacing.
        const only = item.length === 1 ? item[0] : undefined;
        return (
          <li key={index} className="md__li">
            {only?.kind === "paragraph" ? <Runs runs={only.content} /> : <Blocks blocks={item} />}
          </li>
        );
      });
      return block.ordered ? (
        <ol className="md__list" start={block.start === 1 ? undefined : block.start}>
          {items}
        </ol>
      ) : (
        <ul className="md__list">{items}</ul>
      );
    }
    case "table":
      return (
        <div className="md__tablewrap">
          <table className="md__table">
            <thead>
              <tr>
                {block.head.map((cell, index) => (
                  <th key={index}>
                    <Runs runs={cell} />
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {block.rows.map((row, rowIndex) => (
                <tr key={rowIndex}>
                  {row.map((cell, index) => (
                    <td key={index}>
                      <Runs runs={cell} />
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
  }
}

function Runs({ runs }: { readonly runs: readonly Inline[] }): ReactNode {
  return runs.map((run, index) => <RunNode key={index} run={run} />);
}

/** A web address: shown in full, and opened by the runtime on a click. */
function WebLink({
  url,
  children,
  label,
}: {
  readonly url: string;
  readonly children: ReactNode;
  readonly label: string;
}) {
  const { onOpenUrl } = useContext(Hooks);
  const address = label === url ? null : <span className="md__url"> ({url})</span>;
  if (onOpenUrl === undefined) {
    // Shown, not followed. The address is on screen so a person can copy it
    // into a browser that is meant for the web.
    return (
      <span className="md__link" title={url}>
        {children}
        {address}
      </span>
    );
  }
  return (
    <button
      type="button"
      className="md__path md__weblink"
      title={`Open ${url} in your browser`}
      onClick={() => onOpenUrl(url)}
    >
      {children}
      {address}
    </button>
  );
}

function RunNode({ run }: { readonly run: Inline }): ReactNode {
  const { base, onOpenPath, drawImage } = useContext(Hooks);

  switch (run.kind) {
    case "text":
      return run.text;
    case "strong":
      return (
        <strong>
          <Runs runs={run.children} />
        </strong>
      );
    case "em":
      return (
        <em>
          <Runs runs={run.children} />
        </em>
      );
    case "strike":
      return (
        <s>
          <Runs runs={run.children} />
        </s>
      );
    case "code": {
      if (!looksLikePath(run.text)) {
        return <code className="md__code">{run.text}</code>;
      }
      const path = run.text.replace(/\\/g, "/");
      const beside = resolveRelative(base, path);
      const candidates = [resolveRelative("", path), beside].filter(
        (candidate): candidate is string => candidate !== null,
      );
      if (candidates.length === 0) {
        return <code className="md__code">{run.text}</code>;
      }
      return (
        <button
          type="button"
          className="md__path"
          title={`Open ${candidates[0]} in the preview`}
          onClick={() => onOpenPath([...new Set(candidates)])}
        >
          <code className="md__code">{run.text}</code>
        </button>
      );
    }
    case "link": {
      const label = plain(run.children);
      const url = webUrl(run.href);
      if (url !== null) {
        return (
          <WebLink url={url} label={label}>
            <Runs runs={run.children} />
          </WebLink>
        );
      }
      const target = isExternal(run.href) ? null : resolveRelative(base, run.href);
      if (target === null) {
        return (
          <span className="md__link" title={run.href}>
            <Runs runs={run.children} />
            {label === run.href ? null : <span className="md__url"> ({run.href})</span>}
          </span>
        );
      }
      return (
        <button
          type="button"
          className="md__path"
          title={`Open ${target} in the preview`}
          onClick={() => onOpenPath([target])}
        >
          <Runs runs={run.children} />
        </button>
      );
    }
    case "image": {
      const target = isExternal(run.src) ? null : resolveRelative(base, run.src);
      const drawn = drawImage?.({ alt: run.alt, src: run.src, target }) ?? null;
      if (drawn !== null) {
        return drawn;
      }
      const label = run.alt.length > 0 ? `[image: ${run.alt}]` : "[image]";
      // A remote image is never fetched: it is a link to the picture.
      const url = webUrl(run.src);
      if (url !== null) {
        return (
          <WebLink url={url} label={label}>
            <span className="md__image">{label}</span>
          </WebLink>
        );
      }
      if (target === null) {
        return (
          <span className="md__image" title={run.src}>
            {label}
          </span>
        );
      }
      return (
        <button
          type="button"
          className="md__path md__image"
          title={`Open ${target} in the preview`}
          onClick={() => onOpenPath([target])}
        >
          {label}
        </button>
      );
    }
  }
}
