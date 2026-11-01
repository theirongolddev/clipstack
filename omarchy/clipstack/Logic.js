.pragma library

// Pure logic of the ClipStack overlay. Plain functions only: Overlay.qml
// imports this file and tests/logic.test.mjs runs it under node.

var LIST_LIMIT = "10000";
var MINUTE = 60 * 1000;
var HOUR = 60 * MINUTE;
var DAY = 24 * HOUR;

// ------------------------------------------------------------------ parsing

function num(v) {
  return typeof v === "number" && isFinite(v) ? v : 0;
}

// `clipstack list --json` / `search --json` output -> { entries, error }.
// Never throws: a bad payload becomes an error string for the status line.
function parseEntries(text) {
  var raw = String(text || "").trim();
  if (raw === "") return { entries: [], error: "" };
  var data;
  try {
    data = JSON.parse(raw);
  } catch (e) {
    return { entries: [], error: "clipstack returned invalid JSON" };
  }
  if (!Array.isArray(data)) return { entries: [], error: "clipstack output is not a list" };
  var out = [];
  for (var i = 0; i < data.length; i++) {
    var e = data[i];
    if (!e || typeof e.id !== "number") continue;
    out.push({
      id: e.id,
      position: num(e.position),
      createdAt: num(e.created_at),
      lastUsed: num(e.last_used),
      size: num(e.size),
      preview: typeof e.preview === "string" ? e.preview : "",
      pinned: e.pinned === true,
      alias: typeof e.alias === "string" ? e.alias : "",
      kind: e.kind === "image" ? "image" : "text",
      width: num(e.width),
      height: num(e.height),
      mime: typeof e.mime === "string" ? e.mime : "",
    });
  }
  return { entries: out, error: "" };
}

// ------------------------------------------------------------------ ages

function elapsedSeconds(ms, now) {
  return Math.max(0, Math.floor((now - ms) / 1000));
}

// Fixed-width row age: 5s, 2m, 1h, 3d (as the terminal picker).
function compactAge(ms, now) {
  var s = elapsedSeconds(ms, now);
  if (s < 60) return s + "s";
  if (s < 3600) return Math.floor(s / 60) + "m";
  if (s < 86400) return Math.floor(s / 3600) + "h";
  return Math.floor(s / 86400) + "d";
}

// Preview header age: "copied 2 min ago".
function longAge(ms, now) {
  var s = elapsedSeconds(ms, now);
  if (s < 60) return "copied just now";
  if (s < 3600) return "copied " + Math.floor(s / 60) + " min ago";
  if (s < 86400) return "copied " + Math.floor(s / 3600) + " h ago";
  return "copied " + Math.floor(s / 86400) + " d ago";
}

// ------------------------------------------------------------------ grouping

function localMidnight(now) {
  var d = new Date(now);
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

function dayGroup(ms, now) {
  var today = localMidnight(now);
  if (ms >= today) return "TODAY";
  var d = new Date(today);
  var yesterday = new Date(d.getFullYear(), d.getMonth(), d.getDate() - 1).getTime();
  if (ms >= yesterday) return "YESTERDAY";
  return "EARLIER";
}

function copyWith(e, group) {
  var r = {};
  for (var k in e) r[k] = e[k];
  r.group = group;
  return r;
}

// Rows for the list. Browsing: PINNED, then TODAY / YESTERDAY / EARLIER by
// last use. Searching: one MATCHES group in the CLI's order (pinned first,
// then newest).
function buildRows(entries, query, now) {
  var rows = [];
  var i;
  if (String(query || "").trim() !== "") {
    for (i = 0; i < entries.length; i++) rows.push(copyWith(entries[i], "MATCHES"));
    return rows;
  }
  var rest = [];
  for (i = 0; i < entries.length; i++) {
    if (entries[i].pinned) rows.push(copyWith(entries[i], "PINNED"));
    else rest.push(entries[i]);
  }
  rest = rest.slice().sort(function (a, b) { return b.lastUsed - a.lastUsed; });
  for (i = 0; i < rest.length; i++) rows.push(copyWith(rest[i], dayGroup(rest[i].lastUsed, now)));
  return rows;
}

function counts(entries) {
  var pinned = 0;
  for (var i = 0; i < entries.length; i++) if (entries[i].pinned) pinned++;
  return { total: entries.length, pinned: pinned };
}

// ------------------------------------------------------------------ highlight

function queryWords(query) {
  var q = String(query || "").trim().toLowerCase();
  return q === "" ? [] : q.split(/\s+/);
}

// [{ text, match }] covering `text`; every case-insensitive hit of every
// query word is a match span, overlapping hits merged.
function highlightSpans(text, query) {
  text = String(text || "");
  if (text === "") return [];
  var words = queryWords(query);
  var lower = text.toLowerCase();
  if (words.length === 0 || lower.length !== text.length) return [{ text: text, match: false }];

  var marks = [];
  for (var w = 0; w < words.length; w++) {
    var from = 0;
    for (;;) {
      var at = lower.indexOf(words[w], from);
      if (at < 0) break;
      marks.push([at, at + words[w].length]);
      from = at + 1;
    }
  }
  if (marks.length === 0) return [{ text: text, match: false }];
  marks.sort(function (a, b) { return a[0] - b[0]; });
  var merged = [marks[0].slice()];
  for (var m = 1; m < marks.length; m++) {
    var last = merged[merged.length - 1];
    if (marks[m][0] <= last[1]) last[1] = Math.max(last[1], marks[m][1]);
    else merged.push(marks[m].slice());
  }
  var spans = [];
  var pos = 0;
  for (var s = 0; s < merged.length; s++) {
    if (merged[s][0] > pos) spans.push({ text: text.slice(pos, merged[s][0]), match: false });
    spans.push({ text: text.slice(merged[s][0], merged[s][1]), match: true });
    pos = merged[s][1];
  }
  if (pos < text.length) spans.push({ text: text.slice(pos), match: false });
  return spans;
}

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

// StyledText for a row or preview line: escaped text, matches wrapped in
// `open`/`close` (e.g. a <span style=...> pair).
function highlightHtml(text, query, open, close) {
  var spans = highlightSpans(text, query);
  var out = "";
  for (var i = 0; i < spans.length; i++) {
    var t = escapeHtml(spans[i].text);
    out += spans[i].match ? open + t + close : t;
  }
  return out;
}

// ------------------------------------------------------------------ keys

// Qt::Key and Qt::KeyboardModifier values, so this file needs no Qt.
var Key = {
  Escape: 0x01000000, Backspace: 0x01000003, Return: 0x01000004, Enter: 0x01000005,
  Home: 0x01000010, End: 0x01000011, Up: 0x01000013, Down: 0x01000015,
  PageUp: 0x01000016, PageDown: 0x01000017,
  A: 0x41, E: 0x45, J: 0x4a, K: 0x4b, P: 0x50,
};
var Mod = { Shift: 0x02000000, Ctrl: 0x04000000, Alt: 0x08000000, Meta: 0x10000000, Keypad: 0x20000000 };

// The overlay action for a key press, or "" to let the search field have it.
function keyAction(key, modifiers, searching) {
  var mods = modifiers & (Mod.Shift | Mod.Ctrl | Mod.Alt | Mod.Meta);
  if (mods === 0) {
    switch (key) {
      case Key.Escape: return searching ? "clear" : "close";
      case Key.Up: return "up";
      case Key.Down: return "down";
      case Key.PageUp: return "pageUp";
      case Key.PageDown: return "pageDown";
      case Key.Home: return "home";
      case Key.End: return "end";
      case Key.Return: case Key.Enter: return "paste";
    }
    return "";
  }
  if (mods === Mod.Shift) {
    return key === Key.Return || key === Key.Enter ? "copy" : "";
  }
  if (mods === Mod.Ctrl) {
    switch (key) {
      case Key.K: return "up";
      case Key.J: return "down";
      case Key.P: return "pin";
      case Key.A: return "alias";
      case Key.E: return "edit";
      case Key.Backspace: return "delete";
    }
  }
  return "";
}

// ------------------------------------------------------------------ selection

// New selected index for a movement action; clamps at both ends.
function moveIndex(index, count, action, pageSize) {
  if (count <= 0) return -1;
  var i = Math.max(0, index);
  switch (action) {
    case "up": i -= 1; break;
    case "down": i = index < 0 ? 0 : i + 1; break;
    case "pageUp": i -= pageSize; break;
    case "pageDown": i += pageSize; break;
    case "home": i = 0; break;
    case "end": i = count - 1; break;
  }
  return Math.max(0, Math.min(count - 1, i));
}

// After a refresh: the row with the same entry id, else the old index
// clamped into range; -1 for an empty list.
function selectionAfterRefresh(rows, prevId, prevIndex) {
  if (rows.length === 0) return -1;
  for (var i = 0; i < rows.length; i++) if (rows[i].id === prevId) return i;
  return Math.max(0, Math.min(rows.length - 1, prevIndex));
}

// ------------------------------------------------------------------ alias

// The CLI's grammar (`clipstack alias`): [a-z0-9_-]{1,32}, starting with a
// letter or digit; a leading @ is optional.
function validateAlias(input, entries, entryId) {
  var name = String(input || "");
  if (name.charAt(0) === "@") name = name.slice(1);
  var r = function (state, valid, message) {
    return { name: name, state: state, valid: valid, message: message };
  };
  if (name === "") return r("empty", false, "");
  if (name.length > 32) return r("invalid", false, "too long (max 32)");
  if (!/^[a-z0-9]/.test(name)) return r("invalid", false, "start with a letter or digit");
  var bad = name.match(/[^a-z0-9_-]/);
  if (bad) return r("invalid", false, "“" + bad[0] + "” not allowed");
  for (var i = 0; i < entries.length; i++) {
    if (entries[i].alias === name) {
      return entries[i].id === entryId ? r("current", true, "current alias") : r("taken", false, "taken");
    }
  }
  return r("available", true, "available");
}

// ------------------------------------------------------------------ preview

// Lines of an entry; the empty line after a final newline is not counted.
function splitLines(text) {
  var all = String(text || "").split(/\r?\n/);
  if (all.length > 1 && all[all.length - 1] === "") all.pop();
  return all;
}

function previewLines(text, max) {
  var all = splitLines(text);
  return { lines: all.slice(0, max), total: all.length, truncated: all.length > max };
}

function firstMatchLine(lines, query) {
  var words = queryWords(query);
  if (words.length === 0) return -1;
  for (var i = 0; i < lines.length; i++) {
    var l = lines[i].toLowerCase();
    for (var w = 0; w < words.length; w++) if (l.indexOf(words[w]) >= 0) return i;
  }
  return -1;
}

// ------------------------------------------------------------------ labels

function formatSize(bytes) {
  if (bytes < 1024) return bytes + " B";
  if (bytes < 1024 * 1024) {
    var kb = bytes / 1024;
    return (kb < 10 ? kb.toFixed(1) : String(Math.round(kb))) + " KB";
  }
  return (bytes / (1024 * 1024)).toFixed(1) + " MB";
}

var SHELL_WORDS = ["sudo", "ssh", "scp", "rsync", "docker", "podman", "kubectl", "git", "cargo", "rustup",
  "npm", "npx", "pnpm", "yarn", "node", "python", "python3", "pip", "uv", "make", "cd", "ls", "cat",
  "grep", "rg", "curl", "wget", "rm", "cp", "mv", "mkdir", "chmod", "chown", "echo", "export",
  "systemctl", "journalctl", "pacman", "yay", "paru", "omarchy", "clipstack"];
var DOCKER_WORDS = ["FROM", "RUN", "COPY", "ADD", "ENV", "ARG", "WORKDIR", "CMD", "ENTRYPOINT", "EXPOSE",
  "LABEL", "USER", "VOLUME"];
var SQL_WORDS = ["SELECT", "INSERT", "UPDATE", "DELETE", "CREATE", "ALTER", "DROP", "WITH"];
var SOURCE_WORDS = ["fn", "let", "const", "var", "def", "import", "use", "pub", "class", "function",
  "struct", "impl", "package", "#!"];
// Upper-case words tinted anywhere in a code line
var KEYWORDS = DOCKER_WORDS.concat(SQL_WORDS, ["AS", "INTO", "VALUES", "SET", "WHERE", "AND", "OR",
  "NOT", "NULL", "ORDER", "GROUP", "BY", "LIMIT", "JOIN", "ON", "DESC", "ASC", "TABLE", "IN", "IS"]);
var CODE_MARKS = /[{};]\s*$|[{}]|=|\$\(|\$\{|~\/|&&|\|\||\s\|\s|->|=>|::|\w\(\)|\s--?[a-zA-Z]/;

function firstWord(text) {
  var line = String(text || "").split("\n")[0].trim();
  if (line.indexOf("$ ") === 0) return "$";
  return line.split(/\s+/)[0];
}

// "shell", "docker", "sql", "code" or "" from the first word of the text.
function codeFamily(text) {
  var w = firstWord(text);
  if (w === "$" || SHELL_WORDS.indexOf(w) >= 0) return "shell";
  if (DOCKER_WORDS.indexOf(w) >= 0) return "docker";
  if (SQL_WORDS.indexOf(w) >= 0) return "sql";
  if (SOURCE_WORDS.indexOf(w) >= 0 || w.indexOf("#!") === 0) return "code";
  return "";
}

// Rows and previews show code in JetBrains Mono, prose in Noto Sans.
function looksLikeCode(text) {
  var line = String(text || "").split("\n")[0].trim();
  if (line === "") return false;
  return codeFamily(line) !== "" || CODE_MARKS.test(line);
}

function isLink(text) {
  var t = String(text || "").trim();
  if (t === "" || /\s/.test(t)) return false;
  return /^[a-z][a-z0-9+.-]*:\/\/\S+$/i.test(t) || /^www\./i.test(t) ||
    /^[\w-]+(\.[\w-]+)+\/\S*$/.test(t);
}

// The chip in the preview header.
function kindLabel(entry, content) {
  if (entry.kind === "image") return "Image";
  if (isLink(content)) return "Link";
  switch (codeFamily(content)) {
    case "shell": return "Shell command";
    case "docker": return "Dockerfile";
    case "sql": return "SQL";
  }
  return looksLikeCode(content) ? "Code" : "Text";
}

// Calls fn(match, offset) for every match of the global regex `re`.
function eachMatch(text, re, fn) {
  text.replace(re, function (match) {
    fn(match, arguments[arguments.length - 2]);
    return match;
  });
}

// Per-character tint of one code line: 0 plain, 1 keyword, 2 literal.
function tintClasses(line) {
  var cls = [];
  for (var i = 0; i < line.length; i++) cls.push(0);
  var paint = function (from, to, c) {
    for (var k = from; k < to; k++) cls[k] = c;
  };
  eachMatch(line, /"[^"]*"|'[^']*'/g, function (s, at) { paint(at, at + s.length, 2); });
  var first = true;
  eachMatch(line, /\S+/g, function (w, at) {
    var isFirst = first;
    first = false;
    if (cls[at] === 2) return;
    var bare = w.replace(/[^A-Za-z0-9_#!.-]+$/, "");
    if (isFirst && (SHELL_WORDS.indexOf(bare) >= 0 || SOURCE_WORDS.indexOf(bare) >= 0)) {
      paint(at, at + bare.length, 1);
    } else if (KEYWORDS.indexOf(bare) >= 0) {
      paint(at, at + bare.length, 1);
    } else if (/^--?[A-Za-z]/.test(w)) {
      paint(at, at + w.length, 2);
    } else {
      var kv = w.match(/^[A-Z_][A-Z0-9_]*=/);
      if (kv && w.length > kv[0].length) paint(at + kv[0].length, at + w.length, 2);
    }
  });
  return cls;
}

// One preview line as rich text: escaped, lightly tinted when `tint` (the
// entry looks like code), query hits marked on top. `style` holds the
// colours: { keyword, literal, mark, markText }.
function codeHtml(line, query, tint, style) {
  line = String(line || "");
  var cls = tint ? tintClasses(line) : [];
  var marks = [];
  var spans = highlightSpans(line, query);
  for (var s = 0; s < spans.length; s++) {
    for (var c = 0; c < spans[s].text.length; c++) marks.push(spans[s].match);
  }
  var out = "";
  var i = 0;
  while (i < line.length) {
    var t = cls[i] || 0;
    var mk = marks[i] === true;
    var j = i + 1;
    while (j < line.length && (cls[j] || 0) === t && (marks[j] === true) === mk) j++;
    var text = escapeHtml(line.slice(i, j));
    if (mk) out += "<span style=\"background-color:" + style.mark + ";color:" + style.markText + "\">" + text + "</span>";
    else if (t === 1) out += "<span style=\"color:" + style.keyword + "\">" + text + "</span>";
    else if (t === 2) out += "<span style=\"color:" + style.literal + "\">" + text + "</span>";
    else out += text;
    i = j;
  }
  return out;
}

function metaItems(entry, content, now) {
  if (entry.kind === "image") {
    return ["PNG", entry.width + "×" + entry.height, formatSize(entry.size), longAge(entry.lastUsed, now)];
  }
  var lines = splitLines(content).length;
  return [formatSize(entry.size), lines + (lines === 1 ? " line" : " lines"), longAge(entry.lastUsed, now)];
}

function countLabel(shown, total, searching) {
  return searching ? shown + " of " + total : String(total);
}

function footerText(total, pinned) {
  return total + (total === 1 ? " clip" : " clips") + " · " + pinned + " pinned";
}

// ------------------------------------------------------------------ commands

// Every command is an argv array; user input is always its own element.
function listArgv(bin) {
  return [bin, "list", "--json", "-c", LIST_LIMIT];
}

function searchArgv(bin, query) {
  return [bin, "search", "--json", "-c", LIST_LIMIT, "--", query];
}

function contentArgv(bin, id) {
  return [bin, "get", "id:" + id];
}

function imagePathArgv(bin, id) {
  return [bin, "get", "id:" + id, "--image-path"];
}

function actionArgv(bin, action, entry, arg) {
  var ref = "id:" + entry.id;
  switch (action) {
    case "paste": return [bin, "get", ref, "--paste"];
    case "copy": return [bin, "get", ref, "--copy"];
    case "pin": return [bin, entry.pinned ? "unpin" : "pin", ref];
    case "alias": return [bin, "alias", ref, String(arg)];
    case "delete": return [bin, "delete", ref];
    case "edit": return ["omarchy-launch-tui", "--app-id=org.omarchy.clipstack", bin, "edit", ref];
  }
  return [];
}

// Picking an entry closes the overlay, as in Maccy. Paste and edit close it
// before the command runs so focus is back on the previous window (wtype
// types into it; the editor opens over it).
function actionClosesOverlay(action) {
  return action === "paste" || action === "copy" || action === "edit";
}

// "" on success, else one line for the footer status. `search` exits 1 on
// no match, which is not an error.
function commandError(kind, exitCode, stdout, stderr) {
  if (exitCode === 0) return "";
  if (kind === "search" && exitCode === 1) return "";
  var lines = String(stderr || "").split("\n");
  var last = "";
  for (var i = 0; i < lines.length; i++) {
    var l = lines[i].trim();
    var m = l.match(/^(?:error|Error):\s*(.*)$/);
    if (m && m[1] !== "") return m[1];
    if (l !== "") last = l;
  }
  return last !== "" ? last : "clipstack " + kind + " failed (exit " + exitCode + ")";
}
