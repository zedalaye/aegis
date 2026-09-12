/**
 * A markdown file, drawn as elements (PLAN 7.15).
 *
 * The tree comes from `lib/markdown.ts`; this only maps nodes to elements.
 * There is no `dangerouslySetInnerHTML` here and there must never be one: every
 * string ends up as a text node, which is what makes a hostile file inert. See
 * the parser's header for what that costs — no loaded images, no navigating
 * links — and why each is the point.
 *
 * What it does add is the one affordance a preview of a *brief* needs: a path
 * the file names can be opened. A relative link opens the file it points at; a
 * code span that reads as a path offers the same. Both go through the runtime's
 * preview command, which is contained to the workspace whatever the file says.
 */

import { createElement, useMemo } from "react";
import type { ReactNode } from "react";

import {
  isExternal,
  looksLikePath,
  parseMarkdown,
  plain,
  resolveRelative,
} from "../../lib/markdown";
import type { Block, Inline } from "../../lib/markdown";

type Props = {
  readonly source: string;
  /** The folder the file is in, workspace-relative, so its links resolve. */
  readonly base: string;
  /**
   * Opens a workspace file in the preview. Several candidates, tried in order:
   * a brief names its inputs from the workspace root, a README names its
   * neighbours from its own folder, and a code span does not say which.
   */
  readonly onOpenPath: (candidates: readonly string[]) => void;
};

export default function Markdown({ source, base, onOpenPath }: Props) {
  const blocks = useMemo(() => parseMarkdown(source), [source]);

  return (
    <div className="md">
      {blocks.map((block, index) => (
        <BlockNode key={index} block={block} base={base} onOpenPath={onOpenPath} />
      ))}
    </div>
  );
}

function BlockNode({
  block,
  base,
  onOpenPath,
}: {
  readonly block: Block;
  readonly base: string;
  readonly onOpenPath: Props["onOpenPath"];
}) {
  const inline = (runs: readonly Inline[]) => (
    <Runs runs={runs} base={base} onOpenPath={onOpenPath} />
  );
  const blocks = (children: readonly Block[]) =>
    children.map((child, index) => (
      <BlockNode key={index} block={child} base={base} onOpenPath={onOpenPath} />
    ));

  switch (block.kind) {
    case "heading":
      return createElement(
        `h${block.level}`,
        { className: `md__h md__h${block.level}` },
        inline(block.content),
      );
    case "paragraph":
      return <p className="md__p">{inline(block.content)}</p>;
    case "code":
      return (
        <pre className="md__pre" data-info={block.info || undefined}>
          <code>{block.text}</code>
        </pre>
      );
    case "quote":
      return <blockquote className="md__quote">{blocks(block.children)}</blockquote>;
    case "rule":
      return <hr className="md__rule" />;
    case "list": {
      const items = block.items.map((item, index) => {
        // A tight item is one paragraph; drawing it without the paragraph's
        // margins is what keeps a status board's bullets from double-spacing.
        const only = item.length === 1 ? item[0] : undefined;
        return (
          <li key={index} className="md__li">
            {only?.kind === "paragraph" ? inline(only.content) : blocks(item)}
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
                  <th key={index}>{inline(cell)}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {block.rows.map((row, rowIndex) => (
                <tr key={rowIndex}>
                  {row.map((cell, index) => (
                    <td key={index}>{inline(cell)}</td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
  }
}

function Runs({
  runs,
  base,
  onOpenPath,
}: {
  readonly runs: readonly Inline[];
  readonly base: string;
  readonly onOpenPath: Props["onOpenPath"];
}): ReactNode {
  return runs.map((run, index) => (
    <RunNode key={index} run={run} base={base} onOpenPath={onOpenPath} />
  ));
}

function RunNode({
  run,
  base,
  onOpenPath,
}: {
  readonly run: Inline;
  readonly base: string;
  readonly onOpenPath: Props["onOpenPath"];
}): ReactNode {
  const children = (runs: readonly Inline[]) => (
    <Runs runs={runs} base={base} onOpenPath={onOpenPath} />
  );

  switch (run.kind) {
    case "text":
      return run.text;
    case "strong":
      return <strong>{children(run.children)}</strong>;
    case "em":
      return <em>{children(run.children)}</em>;
    case "strike":
      return <s>{children(run.children)}</s>;
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
      if (isExternal(run.href)) {
        // Shown, not followed. The address is on screen so a person can copy
        // it into a browser that is meant for the web.
        return (
          <span className="md__link" title={run.href}>
            {children(run.children)}
            {label === run.href ? null : (
              <span className="md__url"> ({run.href})</span>
            )}
          </span>
        );
      }
      const target = resolveRelative(base, run.href);
      if (target === null) {
        return <span className="md__link">{children(run.children)}</span>;
      }
      return (
        <button
          type="button"
          className="md__path"
          title={`Open ${target} in the preview`}
          onClick={() => onOpenPath([target])}
        >
          {children(run.children)}
        </button>
      );
    }
    case "image": {
      const label = run.alt.length > 0 ? `image: ${run.alt}` : "image";
      const target = isExternal(run.src) ? null : resolveRelative(base, run.src);
      if (target === null) {
        return (
          <span className="md__image" title={run.src}>
            [{label}]
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
          [{label}]
        </button>
      );
    }
  }
}
