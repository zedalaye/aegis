/**
 * A small markdown reader for the file preview and the transcript (PLAN 7.15,
 * 7.20).
 *
 * Parses into a typed tree rendered as React elements and text nodes — no HTML
 * strings, so unknown constructs are just text. The parser is the sanitizer.
 *
 * - Raw HTML is shown as text.
 * - Remote images are never loaded.
 * - Links do not navigate; a workspace link opens that file in the preview, a
 *   web link opens the OS browser through the runtime.
 *
 * A subset of Markdown for briefs, boards, runbooks and replies — not
 * CommonMark.
 */

/** A run of text inside a block. */
export type Inline =
  | { readonly kind: "text"; readonly text: string }
  | { readonly kind: "code"; readonly text: string }
  | { readonly kind: "strong"; readonly children: readonly Inline[] }
  | { readonly kind: "em"; readonly children: readonly Inline[] }
  | { readonly kind: "strike"; readonly children: readonly Inline[] }
  | {
      readonly kind: "link";
      readonly children: readonly Inline[];
      readonly href: string;
    }
  | { readonly kind: "image"; readonly alt: string; readonly src: string };

/** One block of a document. */
export type Block =
  | {
      readonly kind: "heading";
      readonly level: 1 | 2 | 3 | 4 | 5 | 6;
      readonly content: readonly Inline[];
    }
  | { readonly kind: "paragraph"; readonly content: readonly Inline[] }
  | { readonly kind: "code"; readonly text: string; readonly info: string }
  | { readonly kind: "quote"; readonly children: readonly Block[] }
  | {
      readonly kind: "list";
      readonly ordered: boolean;
      readonly start: number;
      readonly items: readonly (readonly Block[])[];
    }
  | { readonly kind: "rule" }
  | {
      readonly kind: "table";
      readonly head: readonly (readonly Inline[])[];
      readonly rows: readonly (readonly (readonly Inline[])[])[];
    };

/** Parses a whole document. */
export function parseMarkdown(source: string): Block[] {
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  return parseBlocks(lines);
}

// ---------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------

const FENCE = /^ {0,3}(`{3,}|~{3,})(.*)$/;
const HEADING = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$/;
const RULE = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;
const QUOTE = /^ {0,3}> ?/;
const INDENTED = /^(?: {4}|\t)/;
const LIST_ITEM = /^( {0,3})([-*+]|(\d{1,9})[.)])(?:[ \t]+|$)/;
const TABLE_SEPARATOR = /^ {0,3}\|?[ \t]*:?-+:?[ \t]*(?:\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$/;

function isBlank(line: string): boolean {
  return line.trim().length === 0;
}

type Marker = {
  readonly ordered: boolean;
  readonly bullet: string;
  readonly start: number;
  /** Columns before the item's content: indent, marker and the space after. */
  readonly offset: number;
};

function listMarker(line: string): Marker | null {
  const match = LIST_ITEM.exec(line);
  if (match === null) {
    return null;
  }
  const indent = match[1] ?? "";
  const marker = match[2] ?? "";
  const number = match[3];
  // "-" alone on a line is an empty item; the content starts one past it.
  const offset = Math.max(match[0].length, indent.length + marker.length + 1);
  return {
    ordered: number !== undefined,
    bullet: number === undefined ? marker : marker.slice(-1),
    start: number === undefined ? 1 : Number.parseInt(number, 10),
    offset,
  };
}

/** Whether a line opens a block that interrupts a paragraph. */
function interrupts(line: string): boolean {
  return (
    FENCE.test(line) ||
    HEADING.test(line) ||
    RULE.test(line) ||
    QUOTE.test(line) ||
    listMarker(line) !== null
  );
}

function leadingSpaces(line: string): number {
  let count = 0;
  for (const char of line) {
    if (char === " ") {
      count += 1;
    } else if (char === "\t") {
      count += 4 - (count % 4);
    } else {
      break;
    }
  }
  return count;
}

/** Removes up to `columns` of leading indentation. */
function dedent(line: string, columns: number): string {
  let removed = 0;
  let index = 0;
  while (index < line.length && removed < columns) {
    const char = line[index];
    if (char === " ") {
      removed += 1;
    } else if (char === "\t") {
      removed += 4 - (removed % 4);
    } else {
      break;
    }
    index += 1;
  }
  return line.slice(index);
}

function parseBlocks(lines: readonly string[]): Block[] {
  const blocks: Block[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i] ?? "";

    if (isBlank(line)) {
      i += 1;
      continue;
    }

    const fence = FENCE.exec(line);
    if (fence !== null) {
      const marker = fence[1] ?? "```";
      const info = (fence[2] ?? "").trim();
      // A backtick fence's info string may not itself hold a backtick; if it
      // does, this was inline code at the start of a paragraph.
      if (!(marker.startsWith("`") && info.includes("`"))) {
        const body: string[] = [];
        i += 1;
        while (i < lines.length && !closesFence(lines[i] ?? "", marker)) {
          body.push(lines[i] ?? "");
          i += 1;
        }
        // Past the closing fence, or past the end of a file that never closed
        // one — which is still a code block rather than a broken page.
        i += 1;
        blocks.push({ kind: "code", text: body.join("\n"), info });
        continue;
      }
    }

    const heading = HEADING.exec(line);
    if (heading !== null) {
      const level = (heading[1] ?? "#").length as 1 | 2 | 3 | 4 | 5 | 6;
      blocks.push({
        kind: "heading",
        level,
        content: parseInline(heading[2] ?? ""),
      });
      i += 1;
      continue;
    }

    if (RULE.test(line)) {
      blocks.push({ kind: "rule" });
      i += 1;
      continue;
    }

    if (INDENTED.test(line)) {
      const body: string[] = [];
      while (
        i < lines.length &&
        (INDENTED.test(lines[i] ?? "") || isBlank(lines[i] ?? ""))
      ) {
        body.push(dedent(lines[i] ?? "", 4));
        i += 1;
      }
      while (body.length > 0 && isBlank(body[body.length - 1] ?? "")) {
        body.pop();
      }
      blocks.push({ kind: "code", text: body.join("\n"), info: "" });
      continue;
    }

    if (QUOTE.test(line)) {
      const inner: string[] = [];
      while (i < lines.length && QUOTE.test(lines[i] ?? "")) {
        inner.push((lines[i] ?? "").replace(QUOTE, ""));
        i += 1;
      }
      blocks.push({ kind: "quote", children: parseBlocks(inner) });
      continue;
    }

    const marker = listMarker(line);
    if (marker !== null) {
      const parsed = parseList(lines, i, marker);
      blocks.push(parsed.block);
      i = parsed.next;
      continue;
    }

    if (line.includes("|") && TABLE_SEPARATOR.test(lines[i + 1] ?? "")) {
      const head = splitRow(line).map(parseInline);
      const rows: Inline[][][] = [];
      i += 2;
      while (
        i < lines.length &&
        !isBlank(lines[i] ?? "") &&
        (lines[i] ?? "").includes("|")
      ) {
        rows.push(splitRow(lines[i] ?? "").map(parseInline));
        i += 1;
      }
      blocks.push({ kind: "table", head, rows });
      continue;
    }

    const paragraph: string[] = [line.trim()];
    i += 1;
    while (
      i < lines.length &&
      !isBlank(lines[i] ?? "") &&
      !interrupts(lines[i] ?? "")
    ) {
      paragraph.push((lines[i] ?? "").trim());
      i += 1;
    }
    blocks.push({ kind: "paragraph", content: parseInline(paragraph.join("\n")) });
  }

  return blocks;
}

function closesFence(line: string, marker: string): boolean {
  const trimmed = line.trim();
  const char = marker.charAt(0);
  return (
    leadingSpaces(line) < 4 &&
    trimmed.length >= marker.length &&
    [...trimmed].every((c) => c === char)
  );
}

/** One list, starting at `start`, and the line after it. */
function parseList(
  lines: readonly string[],
  start: number,
  first: Marker,
): { block: Block; next: number } {
  const items: string[][] = [];
  let current: string[] = [];
  let offset = first.offset;
  let i = start;
  let blankBefore = false;

  while (i < lines.length) {
    const line = lines[i] ?? "";
    const marker = listMarker(line);

    if (
      marker !== null &&
      marker.ordered === first.ordered &&
      marker.bullet === first.bullet &&
      leadingSpaces(line) < offset
    ) {
      if (i !== start) {
        items.push(current);
      }
      current = [line.slice(marker.offset)];
      offset = marker.offset;
      blankBefore = false;
      i += 1;
      continue;
    }

    if (isBlank(line)) {
      current.push("");
      blankBefore = true;
      i += 1;
      continue;
    }

    if (leadingSpaces(line) >= offset) {
      current.push(dedent(line, offset));
      blankBefore = false;
      i += 1;
      continue;
    }

    // A lazy continuation of the item's paragraph: an unindented line that
    // does not start anything of its own, straight after text.
    if (!blankBefore && !interrupts(line)) {
      current.push(line.trim());
      i += 1;
      continue;
    }

    break;
  }
  items.push(current);

  return {
    block: {
      kind: "list",
      ordered: first.ordered,
      start: first.start,
      items: items.map((item) => parseBlocks(item)),
    },
    next: i,
  };
}

function splitRow(line: string): string[] {
  let body = line.trim();
  if (body.startsWith("|")) {
    body = body.slice(1);
  }
  if (body.endsWith("|") && !body.endsWith("\\|")) {
    body = body.slice(0, -1);
  }

  const cells: string[] = [];
  let cell = "";
  for (let i = 0; i < body.length; i += 1) {
    const char = body[i];
    if (char === "\\" && body[i + 1] === "|") {
      cell += "|";
      i += 1;
    } else if (char === "|") {
      cells.push(cell.trim());
      cell = "";
    } else {
      cell += char;
    }
  }
  cells.push(cell.trim());
  return cells;
}

// ---------------------------------------------------------------------------
// Inlines
// ---------------------------------------------------------------------------

const ESCAPABLE = /[!-/:-@[-`{-~]/;
const AUTOLINK = /^<([a-zA-Z][a-zA-Z0-9+.-]{1,31}:[^\s<>]*)>/;
const WORD = /[\p{L}\p{N}]/u;

/** Parses the text of one block into runs. */
export function parseInline(text: string): Inline[] {
  const out: Inline[] = [];
  let buffer = "";
  const flush = () => {
    if (buffer.length > 0) {
      out.push({ kind: "text", text: buffer });
      buffer = "";
    }
  };

  let i = 0;
  while (i < text.length) {
    const char = text.charAt(i);

    if (char === "\\" && ESCAPABLE.test(text.charAt(i + 1))) {
      buffer += text.charAt(i + 1);
      i += 2;
      continue;
    }

    if (char === "`") {
      const run = runLength(text, i, "`");
      const close = findRun(text, i + run, "`", run);
      if (close !== -1) {
        flush();
        let code = text.slice(i + run, close).replace(/\n/g, " ");
        if (code.length > 2 && code.startsWith(" ") && code.endsWith(" ")) {
          code = code.slice(1, -1);
        }
        out.push({ kind: "code", text: code });
        i = close + run;
        continue;
      }
      buffer += "`".repeat(run);
      i += run;
      continue;
    }

    if (char === "!" && text.charAt(i + 1) === "[") {
      const link = linkAt(text, i + 1);
      if (link !== null) {
        flush();
        out.push({ kind: "image", alt: plain(parseInline(link.label)), src: link.href });
        i = link.end;
        continue;
      }
    }

    if (char === "[") {
      const link = linkAt(text, i);
      if (link !== null) {
        flush();
        out.push({
          kind: "link",
          children: parseInline(link.label),
          href: link.href,
        });
        i = link.end;
        continue;
      }
    }

    if (char === "<") {
      const auto = AUTOLINK.exec(text.slice(i));
      if (auto !== null) {
        flush();
        const href = auto[1] ?? "";
        out.push({ kind: "link", children: [{ kind: "text", text: href }], href });
        i += auto[0].length;
        continue;
      }
    }

    if (char === "*" || char === "_" || char === "~") {
      const emphasis = emphasisAt(text, i);
      if (emphasis !== null) {
        flush();
        out.push(emphasis.node);
        i = emphasis.end;
        continue;
      }
    }

    buffer += char;
    i += 1;
  }

  flush();
  return out;
}

function runLength(text: string, at: number, char: string): number {
  let length = 0;
  while (text.charAt(at + length) === char) {
    length += 1;
  }
  return length;
}

/** The next run of exactly `length` of `char` at or after `from`. */
function findRun(text: string, from: number, char: string, length: number): number {
  let i = from;
  while (i < text.length) {
    if (text.charAt(i) === char) {
      const run = runLength(text, i, char);
      if (run === length) {
        return i;
      }
      i += run;
    } else {
      i += 1;
    }
  }
  return -1;
}

/** `[label](href)` starting at the `[`, or `null`. */
function linkAt(
  text: string,
  open: number,
): { label: string; href: string; end: number } | null {
  let depth = 0;
  let close = -1;
  for (let i = open; i < text.length; i += 1) {
    const char = text.charAt(i);
    if (char === "\\") {
      i += 1;
    } else if (char === "[") {
      depth += 1;
    } else if (char === "]") {
      depth -= 1;
      if (depth === 0) {
        close = i;
        break;
      }
    }
  }
  if (close === -1 || text.charAt(close + 1) !== "(") {
    return null;
  }

  let parens = 0;
  let end = -1;
  for (let i = close + 1; i < text.length; i += 1) {
    const char = text.charAt(i);
    if (char === "\\") {
      i += 1;
    } else if (char === "(") {
      parens += 1;
    } else if (char === ")") {
      parens -= 1;
      if (parens === 0) {
        end = i;
        break;
      }
    } else if (char === "\n") {
      return null;
    }
  }
  if (end === -1) {
    return null;
  }

  let href = text.slice(close + 2, end).trim();
  // An optional title after the destination is dropped: nothing here draws it.
  const titled = /^(\S+)\s+["'(].*["')]$/.exec(href);
  if (titled !== null) {
    href = titled[1] ?? href;
  }
  if (href.startsWith("<") && href.endsWith(">")) {
    href = href.slice(1, -1);
  }

  return { label: text.slice(open + 1, close), href, end: end + 1 };
}

/**
 * `**strong**`, `*em*`, `_em_`, `~~strike~~` starting at `at`, or `null`.
 *
 * Underscores only open and close at word boundaries, so `snake_case_names`
 * and `.aegis/skills/inbox_triage` stay what they are. That is the rule that
 * matters most in files full of paths and identifiers.
 */
function emphasisAt(text: string, at: number): { node: Inline; end: number } | null {
  const char = text.charAt(at);
  const run = runLength(text, at, char);

  if (char === "~") {
    if (run !== 2) {
      return null;
    }
    const close = findRun(text, at + 2, "~", 2);
    if (close === -1 || close === at + 2) {
      return null;
    }
    return {
      node: { kind: "strike", children: parseInline(text.slice(at + 2, close)) },
      end: close + 2,
    };
  }

  const before = text.charAt(at - 1);
  if (char === "_" && WORD.test(before)) {
    return null;
  }

  const width = Math.min(run, 3);
  for (let size = width; size >= 1; size -= 1) {
    const inner = at + size;
    if (/\s/.test(text.charAt(inner)) || inner >= text.length) {
      continue;
    }

    let search = inner;
    while (search < text.length) {
      const close = text.indexOf(char.repeat(size), search);
      if (close === -1) {
        break;
      }
      const after = text.charAt(close + size);
      const valid =
        close > inner &&
        !/\s/.test(text.charAt(close - 1)) &&
        !(char === "_" && WORD.test(after)) &&
        // A single marker is not the first half of a double one.
        !(size === 1 && (text.charAt(close + 1) === char || text.charAt(close - 1) === char));
      if (valid) {
        const children = parseInline(text.slice(inner, close));
        const node: Inline =
          size === 3
            ? { kind: "strong", children: [{ kind: "em", children }] }
            : size === 2
              ? { kind: "strong", children }
              : { kind: "em", children };
        return { node, end: close + size };
      }
      search = close + 1;
    }
  }
  return null;
}

/** The text of some runs, with the markup dropped. */
export function plain(runs: readonly Inline[]): string {
  return runs
    .map((run) => {
      switch (run.kind) {
        case "text":
        case "code":
          return run.text;
        case "image":
          return run.alt;
        case "strong":
        case "em":
        case "strike":
        case "link":
          return plain(run.children);
      }
    })
    .join("");
}

// ---------------------------------------------------------------------------
// Paths a document points at
// ---------------------------------------------------------------------------

/** Whether a link's destination leaves the workspace — a scheme, a host, an anchor. */
export function isExternal(href: string): boolean {
  return (
    /^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(href) ||
    href.startsWith("//") ||
    href.startsWith("#") ||
    href.startsWith("/") ||
    href.startsWith("\\")
  );
}

/** The address when a destination is an `http`/`https` link, else `null`. */
export function webUrl(href: string): string | null {
  return /^https?:\/\/[^\s/?#]/i.test(href.trim()) ? href.trim() : null;
}

/**
 * `rel` resolved against the folder `base`, both workspace-relative with `/`.
 *
 * `null` above the workspace root — cosmetic; the runtime enforces
 * containment.
 */
export function resolveRelative(base: string, rel: string): string | null {
  const clean = (rel.split(/[?#]/)[0] ?? "").replace(/\\/g, "/");
  if (clean.length === 0) {
    return null;
  }

  const segments = base.split("/").filter((segment) => segment.length > 0);
  for (const segment of clean.split("/")) {
    if (segment === "" || segment === ".") {
      continue;
    }
    if (segment === "..") {
      if (segments.length === 0) {
        return null;
      }
      segments.pop();
    } else {
      segments.push(segment);
    }
  }
  return segments.length === 0 ? null : segments.join("/");
}

/**
 * Whether an inline code span reads as a path in the workspace.
 *
 * Conservative: no spaces, not absolute, and a separator or an extension.
 */
export function looksLikePath(text: string): boolean {
  if (
    text.length === 0 ||
    text.length > 260 ||
    /\s/.test(text) ||
    isExternal(text) ||
    text.startsWith("~") ||
    text.startsWith("-")
  ) {
    return false;
  }
  if (!/^[\p{L}\p{N}_.@+\-/\\()[\]]+$/u.test(text)) {
    return false;
  }
  return /[/\\]/.test(text) || /\.[A-Za-z][A-Za-z0-9]{0,7}$/.test(text);
}
