import { describe, expect, it } from "vitest";

import {
  isExternal,
  looksLikePath,
  parseInline,
  parseMarkdown,
  plain,
  resolveRelative,
  webUrl,
} from "./markdown";

describe("blocks", () => {
  it("reads headings, paragraphs and rules", () => {
    expect(parseMarkdown("# Title\n\nSome text\nwrapped.\n\n---\n### Deeper ##")).toEqual([
      { kind: "heading", level: 1, content: [{ kind: "text", text: "Title" }] },
      { kind: "paragraph", content: [{ kind: "text", text: "Some text\nwrapped." }] },
      { kind: "rule" },
      { kind: "heading", level: 3, content: [{ kind: "text", text: "Deeper" }] },
    ]);
  });

  it("reads a fenced code block and keeps its body verbatim", () => {
    expect(parseMarkdown("```rust\nfn main() {\n    <b>*x*</b>\n}\n```\nafter")).toEqual([
      { kind: "code", info: "rust", text: "fn main() {\n    <b>*x*</b>\n}" },
      { kind: "paragraph", content: [{ kind: "text", text: "after" }] },
    ]);
  });

  it("draws an unclosed fence as a code block, as a stream arrives", () => {
    expect(parseMarkdown("Here:\n\n```ts\nconst a = 1;\nconst")).toEqual([
      { kind: "paragraph", content: [{ kind: "text", text: "Here:" }] },
      { kind: "code", info: "ts", text: "const a = 1;\nconst" },
    ]);
  });

  it("closes a fence only with the same character, at least as long", () => {
    const [block] = parseMarkdown("````\n```\ninner\n```\n````");
    expect(block).toEqual({ kind: "code", info: "", text: "```\ninner\n```" });
  });

  it("reads tight bullet and ordered lists", () => {
    expect(parseMarkdown("- one\n- two\n\n3. three\n4. four")).toEqual([
      {
        kind: "list",
        ordered: false,
        start: 1,
        items: [
          [{ kind: "paragraph", content: [{ kind: "text", text: "one" }] }],
          [{ kind: "paragraph", content: [{ kind: "text", text: "two" }] }],
        ],
      },
      {
        kind: "list",
        ordered: true,
        start: 3,
        items: [
          [{ kind: "paragraph", content: [{ kind: "text", text: "three" }] }],
          [{ kind: "paragraph", content: [{ kind: "text", text: "four" }] }],
        ],
      },
    ]);
  });

  it("nests a list and a code block inside an item", () => {
    const [list] = parseMarkdown("- outer\n  - inner\n\n  ```\n  code\n  ```");
    expect(list).toEqual({
      kind: "list",
      ordered: false,
      start: 1,
      items: [
        [
          { kind: "paragraph", content: [{ kind: "text", text: "outer" }] },
          {
            kind: "list",
            ordered: false,
            start: 1,
            items: [[{ kind: "paragraph", content: [{ kind: "text", text: "inner" }] }]],
          },
          { kind: "code", info: "", text: "code" },
        ],
      ],
    });
  });

  it("reads a quote and a table", () => {
    expect(parseMarkdown("> quoted\n\n| a | b |\n|---|:-:|\n| 1 | 2 \\| 3 |")).toEqual([
      {
        kind: "quote",
        children: [{ kind: "paragraph", content: [{ kind: "text", text: "quoted" }] }],
      },
      {
        kind: "table",
        head: [[{ kind: "text", text: "a" }], [{ kind: "text", text: "b" }]],
        rows: [[[{ kind: "text", text: "1" }], [{ kind: "text", text: "2 | 3" }]]],
      },
    ]);
  });

  it("keeps raw HTML as text", () => {
    expect(parseMarkdown('<img src=x onerror="alert(1)">\n<script>1</script>')).toEqual([
      {
        kind: "paragraph",
        content: [
          { kind: "text", text: '<img src=x onerror="alert(1)">\n<script>1</script>' },
        ],
      },
    ]);
  });

  it("reads CRLF like LF", () => {
    expect(parseMarkdown("# A\r\n\r\ntext")).toEqual(parseMarkdown("# A\n\ntext"));
  });
});

describe("inlines", () => {
  it("reads emphasis, strong, strike and code", () => {
    expect(parseInline("*a* **b** ~~c~~ `d`")).toEqual([
      { kind: "em", children: [{ kind: "text", text: "a" }] },
      { kind: "text", text: " " },
      { kind: "strong", children: [{ kind: "text", text: "b" }] },
      { kind: "text", text: " " },
      { kind: "strike", children: [{ kind: "text", text: "c" }] },
      { kind: "text", text: " " },
      { kind: "code", text: "d" },
    ]);
  });

  it("leaves underscores inside identifiers alone", () => {
    expect(parseInline("snake_case_name and .aegis/skills/inbox_triage")).toEqual([
      { kind: "text", text: "snake_case_name and .aegis/skills/inbox_triage" },
    ]);
  });

  it("leaves an unclosed emphasis as text", () => {
    expect(parseInline("**half")).toEqual([{ kind: "text", text: "**half" }]);
  });

  it("reads links, autolinks and images", () => {
    expect(parseInline('[doc](docs/a.md "title") <https://x.test> ![alt](img.png)')).toEqual([
      { kind: "link", href: "docs/a.md", children: [{ kind: "text", text: "doc" }] },
      { kind: "text", text: " " },
      {
        kind: "link",
        href: "https://x.test",
        children: [{ kind: "text", text: "https://x.test" }],
      },
      { kind: "text", text: " " },
      { kind: "image", alt: "alt", src: "img.png" },
    ]);
  });

  it("does not read markup inside a code span", () => {
    expect(parseInline("`[x](y) *z*`")).toEqual([{ kind: "code", text: "[x](y) *z*" }]);
  });

  it("honours backslash escapes", () => {
    expect(parseInline("\\*not em\\*")).toEqual([{ kind: "text", text: "*not em*" }]);
  });

  it("flattens runs to text", () => {
    expect(plain(parseInline("**a** [b](c) ![d](e)"))).toBe("a b d");
  });
});

describe("destinations", () => {
  it("tells web links from everything else", () => {
    expect(webUrl("https://example.com/x")).toBe("https://example.com/x");
    expect(webUrl(" HTTP://example.com ")).toBe("HTTP://example.com");
    expect(webUrl("javascript:alert(1)")).toBeNull();
    expect(webUrl("file:///etc/passwd")).toBeNull();
    expect(webUrl("https://")).toBeNull();
    expect(webUrl("docs/a.md")).toBeNull();
  });

  it("marks schemes, hosts, anchors and absolute paths as external", () => {
    for (const href of ["https://a", "mailto:x", "//host", "#top", "/etc", "\\\\server"]) {
      expect(isExternal(href), href).toBe(true);
    }
    expect(isExternal("docs/a.md")).toBe(false);
  });

  it("resolves relative paths and refuses to climb above the root", () => {
    expect(resolveRelative("docs", "../README.md")).toBe("README.md");
    expect(resolveRelative("", "./a/b.md#part")).toBe("a/b.md");
    expect(resolveRelative("", "../x")).toBeNull();
  });

  it("recognizes path-like code spans conservatively", () => {
    expect(looksLikePath("src/lib/markdown.ts")).toBe(true);
    expect(looksLikePath("README.md")).toBe(true);
    expect(looksLikePath("npm run build")).toBe(false);
    expect(looksLikePath("--flag")).toBe(false);
    expect(looksLikePath("https://x.test/a.md")).toBe(false);
  });
});
