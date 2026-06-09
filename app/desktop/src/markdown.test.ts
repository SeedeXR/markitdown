// Tests for the dependency-free Markdown renderer. Run with Node's built-in
// test runner + type stripping (no extra dev deps): `npm test`.
//   node --test --experimental-strip-types src/markdown.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { renderMarkdown } from "./markdown.ts";

test("headings, bold, italic, strikethrough, inline code", () => {
  assert.match(renderMarkdown("# Title"), /<h1>Title<\/h1>/);
  assert.match(renderMarkdown("a **b** c"), /<strong>b<\/strong>/);
  assert.match(renderMarkdown("a *b* c"), /<em>b<\/em>/);
  assert.match(renderMarkdown("~~x~~"), /<del>x<\/del>/);
  assert.match(renderMarkdown("use `cmd`"), /<code>cmd<\/code>/);
});

test("GFM table renders to <table>", () => {
  const html = renderMarkdown("| a | b |\n| --- | --- |\n| 1 | 2 |");
  assert.match(html, /<table>.*<th>a<\/th><th>b<\/th>.*<td>1<\/td><td>2<\/td>.*<\/table>/s);
});

test("inline-code placeholder does not collide with ` N ` text", () => {
  // Regression: a cell like "won 3 games" must survive (the NUL-delimited
  // code placeholder must not be confused with a space-digit-space in text).
  const html = renderMarkdown("| x |\n| --- |\n| won 3 games |");
  assert.match(html, /won 3 games/);
  assert.doesNotMatch(html, /undefined/);
});

test("HTML is escaped (no injection from converted documents)", () => {
  const html = renderMarkdown("<script>alert(1)</script>");
  assert.doesNotMatch(html, /<script>/);
  assert.match(html, /&lt;script&gt;/);
});

test("slide-marker comment becomes a divider, advisory note an aside", () => {
  assert.match(renderMarkdown("<!-- Slide number: 3 -->"), /md-slide-sep">Slide 3</);
  assert.match(renderMarkdown("<!-- scanned; needs OCR -->"), /md-note">scanned; needs OCR/);
});

test("literal <br> in a table cell becomes a line break", () => {
  const html = renderMarkdown("| a |\n| --- |\n| l1<br>l2 |");
  assert.match(html, /l1<br>l2/);
});

test("images (data/https) render; other schemes degrade to text", () => {
  assert.match(renderMarkdown("![x](https://h/i.png)"), /<img alt="x" src="https:\/\/h\/i.png"/);
  assert.match(renderMarkdown("![x](data:image/png;base64,AA)"), /<img /);
  assert.match(renderMarkdown("![x](Picture1.jpg)"), /<em>x<\/em>/); // local file ref
});

test("fenced code block is preserved verbatim and escaped", () => {
  const html = renderMarkdown("```py\nprint('<b>')\n```");
  assert.match(html, /<pre><code class="language-py">print\('&lt;b&gt;'\)<\/code><\/pre>/);
});

test("ordered + unordered lists", () => {
  assert.match(renderMarkdown("- a\n- b"), /<ul><li>a<\/li>\n<li>b<\/li><\/ul>|<ul>\n?<li>a<\/li>/s);
  assert.match(renderMarkdown("1. a\n2. b"), /<ol>/);
});
