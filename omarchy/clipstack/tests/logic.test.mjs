import { test } from "node:test";
import assert from "node:assert/strict";
import { loadLogic } from "./load-logic.mjs";

const L = loadLogic();
// Values built inside the vm context have that realm's prototypes; compare
// them as plain JSON.
const j = (v) => JSON.parse(JSON.stringify(v));

const SEC = 1000;
const MIN = 60 * SEC;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;
// 2026-11-01 15:00 local time
const NOW = new Date(2026, 10, 1, 15, 0, 0).getTime();

function entry(over) {
  return Object.assign(
    { id: 1, position: 1, created_at: NOW, last_used: NOW, size: 5, preview: "hello",
      pinned: false, alias: null, source: "watch", mime: "text/plain", kind: "text",
      width: null, height: null },
    over);
}

// ---------------------------------------------------------------- parsing

test("parseEntries normalises list --json objects", () => {
  const raw = JSON.stringify([
    entry({ id: 7, position: 1, alias: "sig", pinned: true }),
    entry({ id: 9, position: 2, kind: "image", mime: "image/png", width: 1440, height: 900,
            preview: "Image · 1440x900", size: 312000 }),
  ]);
  const r = j(L.parseEntries(raw));
  assert.equal(r.error, "");
  assert.equal(r.entries.length, 2);
  assert.deepEqual(r.entries[0], {
    id: 7, position: 1, createdAt: NOW, lastUsed: NOW, size: 5, preview: "hello",
    pinned: true, alias: "sig", kind: "text", width: 0, height: 0, mime: "text/plain",
  });
  assert.equal(r.entries[1].kind, "image");
  assert.equal(r.entries[1].width, 1440);
  assert.equal(r.entries[1].height, 900);
});

test("parseEntries defaults kind to text when the CLI predates images", () => {
  const raw = JSON.stringify([{ id: 3, position: 1, last_used: NOW, created_at: NOW, size: 1,
                                preview: "x", pinned: false, alias: null, mime: "text/plain" }]);
  const r = j(L.parseEntries(raw));
  assert.equal(r.entries[0].kind, "text");
  assert.equal(r.entries[0].alias, "");
});

test("parseEntries accepts empty output and whitespace as no entries", () => {
  assert.deepEqual(j(L.parseEntries("")), { entries: [], error: "" });
  assert.deepEqual(j(L.parseEntries("[]\n")), { entries: [], error: "" });
});

test("parseEntries reports invalid JSON and non-arrays without throwing", () => {
  assert.equal(j(L.parseEntries("{oops")).entries.length, 0);
  assert.match(L.parseEntries("{oops").error, /invalid JSON/);
  assert.match(L.parseEntries("{\"a\":1}").error, /not a list/);
});

test("parseEntries skips objects without a numeric id", () => {
  const r = j(L.parseEntries(JSON.stringify([{ preview: "x" }, entry({ id: 4 })])));
  assert.deepEqual(r.entries.map((e) => e.id), [4]);
});

// ---------------------------------------------------------------- ages

test("compactAge uses s, m, h and d", () => {
  assert.equal(L.compactAge(NOW - 5 * SEC, NOW), "5s");
  assert.equal(L.compactAge(NOW - 2 * MIN, NOW), "2m");
  assert.equal(L.compactAge(NOW - 59 * MIN, NOW), "59m");
  assert.equal(L.compactAge(NOW - HOUR, NOW), "1h");
  assert.equal(L.compactAge(NOW - 3 * DAY, NOW), "3d");
});

test("compactAge clamps future timestamps to 0s", () => {
  assert.equal(L.compactAge(NOW + 10 * SEC, NOW), "0s");
});

test("longAge reads as a sentence fragment", () => {
  assert.equal(L.longAge(NOW - 10 * SEC, NOW), "copied just now");
  assert.equal(L.longAge(NOW - 2 * MIN, NOW), "copied 2 min ago");
  assert.equal(L.longAge(NOW - 5 * HOUR, NOW), "copied 5 h ago");
  assert.equal(L.longAge(NOW - 2 * DAY, NOW), "copied 2 d ago");
});

// ---------------------------------------------------------------- grouping

test("dayGroup splits on local midnight", () => {
  const midnight = new Date(2026, 10, 1, 0, 0, 0).getTime();
  assert.equal(L.dayGroup(midnight, NOW), "TODAY");
  assert.equal(L.dayGroup(midnight - 1, NOW), "YESTERDAY");
  assert.equal(L.dayGroup(midnight - DAY, NOW), "YESTERDAY");
  assert.equal(L.dayGroup(midnight - DAY - 1, NOW), "EARLIER");
});

test("buildRows lists pinned first, then today, yesterday and earlier", () => {
  const es = [
    { id: 1, pinned: false, lastUsed: NOW - MIN },
    { id: 2, pinned: true, lastUsed: NOW - 3 * DAY },
    { id: 3, pinned: false, lastUsed: NOW - 20 * HOUR },
    { id: 4, pinned: false, lastUsed: NOW - 5 * DAY },
    { id: 5, pinned: true, lastUsed: NOW - HOUR },
  ];
  const rows = j(L.buildRows(es, "", NOW));
  assert.deepEqual(rows.map((r) => [r.id, r.group]), [
    [2, "PINNED"], [5, "PINNED"], [1, "TODAY"], [3, "YESTERDAY"], [4, "EARLIER"],
  ]);
});

test("buildRows puts every search result in one MATCHES group, in CLI order", () => {
  const es = [
    { id: 5, pinned: true, lastUsed: NOW - HOUR },
    { id: 1, pinned: false, lastUsed: NOW - 3 * DAY },
  ];
  const rows = j(L.buildRows(es, "  dock ", NOW));
  assert.deepEqual(rows.map((r) => [r.id, r.group]), [[5, "MATCHES"], [1, "MATCHES"]]);
});

test("buildRows does not modify its input", () => {
  const es = [{ id: 1, pinned: false, lastUsed: NOW }];
  L.buildRows(es, "", NOW);
  assert.equal(es[0].group, undefined);
});

test("counts totals and pinned entries", () => {
  assert.deepEqual(j(L.counts([{ pinned: true }, { pinned: false }, { pinned: true }])),
                   { total: 3, pinned: 2 });
});

// ---------------------------------------------------------------- highlight

test("highlightSpans marks every case-insensitive hit of every query word", () => {
  const spans = j(L.highlightSpans("Docker docs: dockerfile", "DOCK"));
  assert.deepEqual(spans.filter((s) => s.match).map((s) => s.text), ["Dock", "dock"]);
  assert.equal(spans.map((s) => s.text).join(""), "Docker docs: dockerfile");
});

test("highlightSpans merges overlapping words", () => {
  const spans = j(L.highlightSpans("compose up", "compo pose"));
  assert.deepEqual(spans, [{ text: "compose", match: true }, { text: " up", match: false }]);
});

test("highlightSpans without a query is one plain span", () => {
  assert.deepEqual(j(L.highlightSpans("abc", "  ")), [{ text: "abc", match: false }]);
  assert.deepEqual(j(L.highlightSpans("", "x")), []);
});

test("highlightHtml escapes text and wraps matches", () => {
  const html = L.highlightHtml("<a> & dock", "dock", "[", "]");
  assert.equal(html, "&lt;a&gt; &amp; [dock]");
});

test("escapeHtml escapes quotes and keeps spaces", () => {
  assert.equal(L.escapeHtml("a \"b\" 'c' <d>"), "a &quot;b&quot; &#39;c&#39; &lt;d&gt;");
});

// ---------------------------------------------------------------- keys

const K = { Up: 0x01000013, Down: 0x01000015, PageUp: 0x01000016, PageDown: 0x01000017,
            Home: 0x01000010, End: 0x01000011, Return: 0x01000004, Enter: 0x01000005,
            Escape: 0x01000000, Backspace: 0x01000003, Delete: 0x01000007, J: 0x4a, K: 0x4b, P: 0x50, A: 0x41,
            E: 0x45, X: 0x58 };
const SHIFT = 0x02000000, CTRL = 0x04000000, ALT = 0x08000000, KEYPAD = 0x20000000;

test("keyAction maps the movement keys", () => {
  assert.equal(L.keyAction(K.Up, 0, false), "up");
  assert.equal(L.keyAction(K.Down, 0, false), "down");
  assert.equal(L.keyAction(K.K, CTRL, false), "up");
  assert.equal(L.keyAction(K.J, CTRL, false), "down");
  assert.equal(L.keyAction(K.PageUp, 0, false), "pageUp");
  assert.equal(L.keyAction(K.PageDown, 0, false), "pageDown");
  assert.equal(L.keyAction(K.Home, 0, false), "home");
  assert.equal(L.keyAction(K.End, 0, false), "end");
});

test("keyAction maps the entry actions", () => {
  assert.equal(L.keyAction(K.Return, 0, false), "paste");
  assert.equal(L.keyAction(K.Enter, KEYPAD, false), "paste");
  assert.equal(L.keyAction(K.Return, SHIFT, false), "copy");
  assert.equal(L.keyAction(K.Enter, SHIFT | KEYPAD, false), "copy");
  assert.equal(L.keyAction(K.P, CTRL, false), "pin");
  assert.equal(L.keyAction(K.A, CTRL, false), "alias");
  assert.equal(L.keyAction(K.E, CTRL, false), "edit");
  assert.equal(L.keyAction(K.Delete, CTRL, false), "delete");
});

test("keyAction: Esc clears a search first, then closes", () => {
  assert.equal(L.keyAction(K.Escape, 0, true), "clear");
  assert.equal(L.keyAction(K.Escape, 0, false), "close");
});

test("keyAction ignores key repeat for actions that close the overlay", () => {
  // Holding Enter to save an alias must not paste once focus is back in search
  assert.equal(L.keyAction(K.Return, 0, false, true), "");
  assert.equal(L.keyAction(K.Return, SHIFT, false, true), "");
  assert.equal(L.keyAction(K.E, CTRL, false, true), "");
  assert.equal(L.keyAction(K.A, CTRL, false, true), "");
  assert.equal(L.keyAction(K.Down, 0, false, true), "down");
  assert.equal(L.keyAction(K.Delete, CTRL, false, true), "delete");
});

test("keyAction leaves typing and unknown chords to the search field", () => {
  assert.equal(L.keyAction(K.J, 0, false), "");
  assert.equal(L.keyAction(K.P, SHIFT, false), "");
  assert.equal(L.keyAction(K.Backspace, 0, true), "");
  // Ctrl+Backspace deletes a word in the search field; plain Delete too
  assert.equal(L.keyAction(K.Backspace, CTRL, true), "");
  assert.equal(L.keyAction(K.Backspace, CTRL, false), "");
  assert.equal(L.keyAction(K.Delete, 0, true), "");
  assert.equal(L.keyAction(K.X, CTRL, false), "");
  assert.equal(L.keyAction(K.P, CTRL | ALT, false), "");
  assert.equal(L.keyAction(K.Return, CTRL, false), "");
});

// ---------------------------------------------------------------- selection

test("moveIndex clamps instead of wrapping", () => {
  assert.equal(L.moveIndex(0, 5, "up", 6), 0);
  assert.equal(L.moveIndex(4, 5, "down", 6), 4);
  assert.equal(L.moveIndex(2, 5, "down", 6), 3);
  assert.equal(L.moveIndex(1, 20, "pageDown", 6), 7);
  assert.equal(L.moveIndex(18, 20, "pageDown", 6), 19);
  assert.equal(L.moveIndex(3, 20, "pageUp", 6), 0);
  assert.equal(L.moveIndex(7, 20, "home", 6), 0);
  assert.equal(L.moveIndex(7, 20, "end", 6), 19);
  assert.equal(L.moveIndex(0, 0, "down", 6), -1);
});

test("selectionAfterRefresh keeps the same entry id", () => {
  const rows = [{ id: 4 }, { id: 9 }, { id: 2 }];
  assert.equal(L.selectionAfterRefresh(rows, 9, 0), 1);
});

test("selectionAfterRefresh falls back to the old index, clamped", () => {
  const rows = [{ id: 4 }, { id: 9 }];
  assert.equal(L.selectionAfterRefresh(rows, 77, 1), 1);
  assert.equal(L.selectionAfterRefresh(rows, 77, 5), 1);
  assert.equal(L.selectionAfterRefresh(rows, -1, -1), 0);
  assert.equal(L.selectionAfterRefresh([], 4, 0), -1);
});

// ---------------------------------------------------------------- alias

test("validateAlias strips a leading @ and accepts a free name", () => {
  const r = j(L.validateAlias("@dcu", [], 3));
  assert.deepEqual(r, { name: "dcu", state: "available", valid: true, message: "available" });
});

test("validateAlias rejects names outside the CLI grammar", () => {
  for (const bad of ["Dcu", "a b", "-lead", "_lead", "x".repeat(33), "dc!", "@@x"]) {
    const r = j(L.validateAlias(bad, [], 3));
    assert.equal(r.state, "invalid", bad);
    assert.equal(r.valid, false, bad);
  }
  assert.equal(L.validateAlias("x".repeat(32), [], 3).state, "available");
  assert.equal(L.validateAlias("a-b_9", [], 3).state, "available");
});

test("validateAlias explains too-long names", () => {
  assert.match(L.validateAlias("x".repeat(33), [], 3).message, /32/);
});

test("validateAlias says taken when another entry owns it", () => {
  const entries = [{ id: 5, alias: "sig" }, { id: 3, alias: "" }];
  const r = j(L.validateAlias("sig", entries, 3));
  assert.equal(r.state, "taken");
  assert.equal(r.valid, false);
  assert.match(r.message, /taken/);
});

test("validateAlias accepts the entry's own alias as current", () => {
  const r = j(L.validateAlias("sig", [{ id: 5, alias: "sig" }], 5));
  assert.equal(r.state, "current");
  assert.equal(r.valid, true);
});

test("validateAlias on empty input is not valid and says nothing", () => {
  assert.deepEqual(j(L.validateAlias("@", [], 1)), { name: "", state: "empty", valid: false, message: "" });
});

// ---------------------------------------------------------------- preview

test("previewLines caps at the limit and counts the total", () => {
  const r = j(L.previewLines("a\nb\nc\nd", 2));
  assert.deepEqual(r, { lines: ["a", "b"], total: 4, truncated: true });
  assert.deepEqual(j(L.previewLines("one", 2000)), { lines: ["one"], total: 1, truncated: false });
  assert.deepEqual(j(L.previewLines("", 2000)), { lines: [""], total: 1, truncated: false });
});

test("previewLines ignores the empty line after a final newline", () => {
  assert.deepEqual(j(L.previewLines("a\nb\n", 10)), { lines: ["a", "b"], total: 2, truncated: false });
  assert.deepEqual(j(L.previewLines("\n", 10)).lines, [""]);
});

test("previewLines treats CRLF as one break", () => {
  assert.deepEqual(j(L.previewLines("a\r\nb", 10)).lines, ["a", "b"]);
});

test("firstMatchLine finds the first line with any query word", () => {
  assert.equal(L.firstMatchLine(["FROM rust", "COPY Dockerfile ./"], "dock"), 1);
  assert.equal(L.firstMatchLine(["abc"], "zz"), -1);
  assert.equal(L.firstMatchLine(["abc"], ""), -1);
});

// ---------------------------------------------------------------- labels

test("formatSize matches the design", () => {
  assert.equal(L.formatSize(28), "28 B");
  assert.equal(L.formatSize(1536), "1.5 KB");
  assert.equal(L.formatSize(312 * 1024), "312 KB");
  assert.equal(L.formatSize(3 * 1024 * 1024 + 300 * 1024), "3.3 MB");
});

test("kindLabel names images, links, shell, Dockerfile, SQL, code and text", () => {
  assert.equal(L.kindLabel({ kind: "image" }, ""), "Image");
  assert.equal(L.kindLabel({ kind: "text" }, "https://docs.docker.com/compose"), "Link");
  assert.equal(L.kindLabel({ kind: "text" }, "docker compose up -d --build"), "Shell command");
  assert.equal(L.kindLabel({ kind: "text" }, "$ ls -la"), "Shell command");
  assert.equal(L.kindLabel({ kind: "text" }, "FROM rust:1.89 AS build\nRUN make"), "Dockerfile");
  assert.equal(L.kindLabel({ kind: "text" }, "SELECT id FROM entries"), "SQL");
  assert.equal(L.kindLabel({ kind: "text" }, "fn main() {\n}"), "Code");
  assert.equal(L.kindLabel({ kind: "text" }, "Best regards, Sam Rivera"), "Text");
});

test("looksLikeCode spots commands and code but not prose", () => {
  for (const s of ["ssh -R ~/.clipstack.sock:host", "FROM rust:1.89 AS build",
                   "SELECT id, preview FROM entries", "fn main() {", "x = 1;"]) {
    assert.equal(L.looksLikeCode(s), true, s);
  }
  for (const s of ["Best regards, Sam Rivera", "Thanks for the quick turnaround. I'll check.",
                   "docs.docker.com/compose/how-tos/"]) {
    assert.equal(L.looksLikeCode(s), false, s);
  }
});

test("metaItems for text: size, lines and age", () => {
  const e = { kind: "text", size: 28, lastUsed: NOW - 2 * MIN };
  assert.deepEqual(j(L.metaItems(e, "one line", NOW)), ["28 B", "1 line", "copied 2 min ago"]);
  assert.deepEqual(j(L.metaItems(e, "a\nb\nc", NOW))[1], "3 lines");
});

test("metaItems does not count the line after a final newline", () => {
  const e = { kind: "text", size: 4, lastUsed: NOW };
  assert.equal(j(L.metaItems(e, "a\nb\n", NOW))[1], "2 lines");
});

test("metaItems for an image: format, dimensions, size and age", () => {
  const e = { kind: "image", size: 312 * 1024, width: 1440, height: 900, lastUsed: NOW - 5 * HOUR };
  assert.deepEqual(j(L.metaItems(e, "", NOW)), ["PNG", "1440×900", "312 KB", "copied 5 h ago"]);
});

test("countLabel and footerText", () => {
  assert.equal(L.countLabel(99, 99, false), "99");
  assert.equal(L.countLabel(3, 99, true), "3 of 99");
  assert.equal(L.footerText(99, 2), "99 clips · 2 pinned");
  assert.equal(L.footerText(1, 0), "1 clip · 0 pinned");
});

// ---------------------------------------------------------------- commands

test("argv builders put user input in its own argument", () => {
  assert.deepEqual(j(L.listArgv("clipstack")), ["clipstack", "list", "--json", "-c", "10000"]);
  assert.deepEqual(j(L.searchArgv("clipstack", "-rf; rm $(x)")),
                   ["clipstack", "search", "--json", "-c", "10000", "--", "-rf; rm $(x)"]);
  assert.deepEqual(j(L.contentArgv("cs", 7)), ["cs", "get", "id:7"]);
  assert.deepEqual(j(L.imagePathArgv("cs", 7)), ["cs", "get", "id:7", "--image-path"]);
});

test("fileUrl percent-encodes each path segment", () => {
  assert.equal(L.fileUrl("/run/user/1000/clipstack-images/6.png"), "file:///run/user/1000/clipstack-images/6.png");
  assert.equal(L.fileUrl("/tmp/a b#1 %20?x/ü.png"), "file:///tmp/a%20b%231%20%2520%3Fx/%C3%BC.png");
  assert.equal(L.fileUrl(""), "");
});

test("actionArgv builds each entry action", () => {
  const e = { id: 7, pinned: false };
  assert.deepEqual(j(L.actionArgv("cs", "paste", e)), ["cs", "get", "id:7", "--paste"]);
  assert.deepEqual(j(L.actionArgv("cs", "copy", e)), ["cs", "get", "id:7", "--copy"]);
  assert.deepEqual(j(L.actionArgv("cs", "pin", e)), ["cs", "pin", "id:7"]);
  assert.deepEqual(j(L.actionArgv("cs", "pin", { id: 7, pinned: true })), ["cs", "unpin", "id:7"]);
  assert.deepEqual(j(L.actionArgv("cs", "alias", e, "dcu")), ["cs", "alias", "id:7", "dcu"]);
  assert.deepEqual(j(L.actionArgv("cs", "delete", e)), ["cs", "delete", "id:7"]);
  assert.deepEqual(j(L.actionArgv("cs", "edit", e)),
                   ["omarchy-launch-tui", "--app-id=org.omarchy.clipstack", "cs", "edit", "id:7"]);
  assert.deepEqual(j(L.actionArgv("cs", "nope", e)), []);
});

test("actionClosesOverlay: paste, copy and edit close (Maccy), the rest stay open", () => {
  assert.equal(L.actionClosesOverlay("paste"), true);
  assert.equal(L.actionClosesOverlay("edit"), true);
  assert.equal(L.actionClosesOverlay("copy"), true);
  assert.equal(L.actionClosesOverlay("pin"), false);
  assert.equal(L.actionClosesOverlay("alias"), false);
  assert.equal(L.actionClosesOverlay("delete"), false);
});

test("commandError: success, search no-match and failures", () => {
  assert.equal(L.commandError("list", 0, "[]", ""), "");
  assert.equal(L.commandError("search", 1, "[]\n", ""), "");
  assert.equal(L.commandError("pin", 1, "", "warning: x\nno entry for id:9\n"), "no entry for id:9");
  assert.equal(L.commandError("delete", 4, "", ""), "clipstack delete failed (exit 4)");
  assert.equal(L.commandError("list", 127, "", ""), "clipstack list failed (exit 127)");
});

test("commandError: search exit 1 with a message is a failure, not a no-match", () => {
  // Exit 1 is also "any failure not listed" (README exit codes); a no-match
  // is silent
  assert.equal(L.commandError("search", 1, "", "Error: Failed to create storage dir: \"/x\"\n\nCaused by:\n    No such file\n"),
               "Failed to create storage dir: \"/x\"");
  assert.equal(L.commandError("search", 1, "[]\n", "\n"), "");
});

test("commandError takes the Error: line clipstack prints, not its causes", () => {
  assert.equal(L.commandError("alias", 1, "", "Error: alias @sig already used by id:5\n"),
               "alias @sig already used by id:5");
  assert.equal(L.commandError("list", 4, "", "Error: cannot open the store\n\nCaused by:\n    locked\n"),
               "cannot open the store");
  assert.equal(L.commandError("get", 2, "", "error: unexpected argument '--image-path' found\n\nUsage: x\n"),
               "unexpected argument '--image-path' found");
});

// ---------------------------------------------------------------- code tint

const TINT = { keyword: "K", literal: "L", mark: "M", markText: "T" };
// Render codeHtml with readable tags: <K>..</K> keyword, <L>..</L> literal,
// [..] mark.
function tinted(line, query, tint) {
  return L.codeHtml(line, query, tint, TINT)
    .replace(/<span style="color:K">(.*?)<\/span>/g, "<K>$1</K>")
    .replace(/<span style="color:L">(.*?)<\/span>/g, "<L>$1</L>")
    .replace(/<span style="background-color:M;color:T">(.*?)<\/span>/g, "[$1]");
}

test("codeHtml tints a leading command and its flags", () => {
  assert.equal(tinted("docker compose up -d --build", "", true),
               "<K>docker</K> compose up <L>-d</L> <L>--build</L>");
});

test("codeHtml tints Dockerfile and SQL keywords, quoted strings and KEY=value values", () => {
  assert.equal(tinted("FROM rust:1.89 AS build", "", true), "<K>FROM</K> rust:1.89 <K>AS</K> build");
  assert.equal(tinted("ENV CARGO_TERM_COLOR=always", "", true), "<K>ENV</K> CARGO_TERM_COLOR=<L>always</L>");
  assert.equal(tinted('CMD ["api"]', "", true), "<K>CMD</K> [<L>&quot;api&quot;</L>]");
});

test("codeHtml without tint only escapes", () => {
  assert.equal(tinted("a <b> -d", "", false), "a &lt;b&gt; -d");
});

test("codeHtml marks query hits on top of the tint", () => {
  assert.equal(tinted("COPY Dockerfile ./", "dock", true), "<K>COPY</K> [Dock]erfile ./");
});

test("codeHtml keeps a word that is part keyword, part mark, in order", () => {
  const html = L.codeHtml("docker up", "ock", true, TINT);
  assert.equal(html.replace(/<[^>]+>/g, ""), "docker up");
  assert.match(html, /background-color:M;color:T">ock</);
});
