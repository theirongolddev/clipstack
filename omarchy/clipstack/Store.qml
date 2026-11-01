import QtQuick
import Quickshell
import "Logic.js" as Logic

// The overlay's state and its clipstack processes. Every byte comes from the
// CLI; nothing here captures the clipboard. Overlay.qml shows it in a layer
// surface; Reader.qml draws it.
Item {
  id: root

  readonly property string bin: Quickshell.env("CLIPSTACK_BIN") || "clipstack"
  readonly property Colors c: Colors {}

  property bool opened: false
  property string query: ""
  readonly property bool searching: root.query.trim() !== ""

  property var allEntries: []      // every entry, from `list --json`
  property var rows: []            // what the list shows (grouped rows)
  property int selectedIndex: -1
  readonly property var selected: root.selectedIndex >= 0 && root.selectedIndex < root.rows.length
    ? root.rows[root.selectedIndex] : null
  property bool resetSelection: true
  property bool keepIndex: false   // keep the row position, not the entry
  property bool loaded: false
  // The rows answer the current query, from a list read since this open:
  // keys typed ahead (Enter right after typing, or Down Enter right after
  // opening) wait for them instead of acting on the rows on screen
  property bool listed: false
  property string rowsQuery: ""
  readonly property bool rowsCurrent: root.listed && root.rowsQuery === root.query
  property string deferredAction: ""
  property real now: Date.now()

  property var imagePaths: ({})    // entry id -> file URL (null = asked, "" = failed)
  property var contentCache: ({})  // entry id -> text
  property var contentOrder: []
  property string previewText: ""
  property int previewId: -1

  property bool aliasOpen: false
  property string aliasInput: ""
  readonly property var aliasCheck: root.selected
    ? Logic.validateAlias(root.aliasInput, root.allEntries, root.selected.id)
    : Logic.validateAlias("", [], -1)

  // An entry action in flight: "running", then "refreshing" until the list
  // shows its result. Other actions wait, so a key repeat never acts on a
  // row that is already gone or changed.
  property string pendingAction: ""

  property string status: ""       // last error, shown in the footer
  property bool statusWhileClosed: false

  readonly property var totals: Logic.counts(root.allEntries)

  // ------------------------------------------------------------ shell API

  function open(payloadJson) {
    root.opened = true;
    root.query = "";
    root.aliasOpen = false;
    root.aliasInput = "";
    if (!root.statusWhileClosed) root.status = "";
    root.statusWhileClosed = false;
    root.pendingAction = "";
    root.deferredAction = "";
    root.listed = false;
    root.forgetFailedImages();
    // The last session may have closed on a search: show the history as
    // last listed until the fresh list arrives. A Down pressed before then
    // still means the second row of the fresh list.
    root.resetSelection = true;
    if (root.loaded) root.showRows(root.allEntries);
    root.resetSelection = false;
    root.keepIndex = true;
    root.refresh();
    root.focusSearch();
  }

  function close() {
    root.deferredAction = "";
    root.aliasOpen = false;
    root.opened = false;
  }

  function toggle() {
    if (root.opened) root.close();
    else root.open("{}");
  }

  // ------------------------------------------------------------ data

  signal focusSearchRequested()
  signal focusAliasRequested()

  function focusSearch() {
    Qt.callLater(function () { root.focusSearchRequested(); });
  }

  function setQuery(text) {
    if (text === root.query) return;
    root.query = text;
    root.resetSelection = true;
    searchDebounce.restart();
  }

  function refresh() {
    root.now = Date.now();
    listCall.run(Logic.listArgv(root.bin), "list");
  }

  // The rows for the current query: a search, or the whole list already
  // loaded (typing never re-reads the list)
  function runQuery() {
    if (root.searching) searchCall.run(Logic.searchArgv(root.bin, root.query.trim()), root.query);
    else root.showRows(root.allEntries);
  }

  function showRows(entries) {
    var prevId = root.selected ? root.selected.id : -1;
    var prevIndex = root.selectedIndex;
    root.now = Date.now();
    root.rows = Logic.buildRows(entries, root.query, root.now);
    root.selectedIndex = root.resetSelection
      ? (root.rows.length > 0 ? 0 : -1)
      : Logic.selectionAfterRefresh(root.rows, root.keepIndex ? -1 : prevId, prevIndex);
    root.resetSelection = false;
    root.keepIndex = false;
    root.rowsQuery = root.query;
    root.loaded = true;
    if (root.pendingAction === "refreshing" && !listCall.busy) root.pendingAction = "";
    if (root.deferredAction !== "" && root.rowsCurrent) {
      const action = root.deferredAction;
      root.deferredAction = "";
      root.act(action);
    }
  }

  function report(error) {
    if (error === "") return;
    root.status = error;
    root.statusWhileClosed = !root.opened;
  }

  function requestImagePath(id) {
    if (root.imagePaths[id] !== undefined) return;
    let next = Object.assign({}, root.imagePaths);
    next[id] = null; // requested
    root.imagePaths = next;
    imageCall.run(Logic.imagePathArgv(root.bin, id), id);
  }

  // A failed --image-path (say, a locked database) is tried again on the
  // next open instead of staying "unavailable" for the shell's lifetime
  function forgetFailedImages() {
    var next = {};
    for (const id in root.imagePaths) if (root.imagePaths[id] !== "") next[id] = root.imagePaths[id];
    root.imagePaths = next;
  }

  function imageUrl(id) {
    var p = root.imagePaths[id];
    return typeof p === "string" ? p : "";
  }

  function loadPreview() {
    var e = root.selected;
    if (!e) {
      root.previewId = -1;
      root.previewText = "";
      return;
    }
    if (e.kind === "image") {
      root.previewId = e.id;
      root.previewText = "";
      root.requestImagePath(e.id);
      return;
    }
    if (root.contentCache[e.id] !== undefined) {
      root.previewId = e.id;
      root.previewText = root.contentCache[e.id];
      return;
    }
    previewDebounce.restart();
  }

  function remember(id, text) {
    var cache = Object.assign({}, root.contentCache);
    var order = root.contentOrder.filter(function (k) { return k !== id; });
    cache[id] = text;
    order.push(id);
    while (order.length > 64) delete cache[order.shift()];
    root.contentCache = cache;
    root.contentOrder = order;
  }

  // ------------------------------------------------------------ actions

  function select(index) {
    if (index < 0 || index >= root.rows.length) return;
    root.selectedIndex = index;
  }

  function move(action, pageSize) {
    var i = Logic.moveIndex(root.selectedIndex, root.rows.length, action, pageSize);
    if (i !== root.selectedIndex) root.selectedIndex = i;
  }

  function act(action) {
    if (root.pendingAction !== "") return;
    if (!root.rowsCurrent) {
      root.deferredAction = action;
      return;
    }
    var e = root.selected;
    if (!e) return;
    if (action === "alias") {
      root.aliasInput = e.alias;
      root.aliasOpen = true;
      Qt.callLater(function () { root.focusAliasRequested(); });
      return;
    }
    if (action === "edit" && e.kind !== "text") return;
    var argv = Logic.actionArgv(root.bin, action, e);
    if (argv.length === 0) return;
    root.status = "";
    if (Logic.actionClosesOverlay(action)) {
      root.close();
      // Let the layer surface unmap so focus is back on the previous window
      handoff.job = { action: action, argv: argv };
      handoff.restart();
      return;
    }
    root.runAction(argv, action);
  }

  function runAction(argv, action) {
    root.pendingAction = "running";
    actionCall.run(argv, action);
  }

  function saveAlias() {
    var e = root.selected;
    var check = root.aliasCheck;
    if (!e || !check.valid || root.pendingAction !== "") return;
    root.cancelAlias();
    if (check.state === "current") return;
    root.status = "";
    root.runAction(Logic.actionArgv(root.bin, "alias", e, check.name), "alias");
  }

  function cancelAlias() {
    root.aliasOpen = false;
    root.aliasInput = "";
    root.focusSearch();
  }

  readonly property int selectedId: root.selected ? root.selected.id : -1
  onSelectedIdChanged: {
    if (root.aliasOpen) root.cancelAlias();
  }
  onSelectedChanged: root.loadPreview()

  // ------------------------------------------------------------ processes

  Timer {
    id: searchDebounce
    interval: 60
    onTriggered: root.runQuery()
  }

  Timer {
    id: previewDebounce
    interval: 30
    onTriggered: {
      var e = root.selected;
      if (e && e.kind === "text") previewCall.run(Logic.contentArgv(root.bin, e.id), e.id);
    }
  }

  Timer {
    id: handoff
    property var job: null
    interval: 60
    onTriggered: {
      if (handoff.job === null) return;
      var job = handoff.job;
      handoff.job = null;
      // The editor runs in a terminal for as long as it is open, so it is
      // started detached; paste and copy report errors like any action.
      if (job.action === "edit") Quickshell.execDetached(job.argv);
      else root.runAction(job.argv, job.action);
    }
  }

  CliCall {
    id: listCall
    onFinished: function (tag, exitCode, stdout, stderr) {
      var error = Logic.commandError("list", exitCode, stdout, stderr);
      var parsed = Logic.parseEntries(error === "" ? stdout : "");
      root.report(error || parsed.error);
      if (error === "" && parsed.error === "") root.allEntries = parsed.entries;
      root.listed = true;
      root.runQuery();
    }
  }

  CliCall {
    id: searchCall
    onFinished: function (tag, exitCode, stdout, stderr) {
      if (tag !== root.query) return; // the query moved on
      var error = Logic.commandError("search", exitCode, stdout, stderr);
      var parsed = Logic.parseEntries(error === "" ? stdout : "");
      root.report(error || parsed.error);
      root.showRows(parsed.entries);
    }
  }

  CliCall {
    id: previewCall
    onFinished: function (tag, exitCode, stdout, stderr) {
      var error = Logic.commandError("get", exitCode, stdout, stderr);
      if (error !== "") {
        root.report(error);
        return;
      }
      root.remember(tag, stdout);
      if (root.selected && root.selected.id === tag) {
        root.previewId = tag;
        root.previewText = stdout;
      }
    }
  }

  CliCall {
    id: imageCall
    latestOnly: false
    onFinished: function (tag, exitCode, stdout, stderr) {
      var error = Logic.commandError("get", exitCode, stdout, stderr);
      var path = stdout.trim();
      let next = Object.assign({}, root.imagePaths);
      next[tag] = error === "" && path !== "" ? Logic.fileUrl(path) : "";
      root.imagePaths = next;
      root.report(error);
    }
  }

  CliCall {
    id: actionCall
    latestOnly: false
    onFinished: function (tag, exitCode, stdout, stderr) {
      root.report(Logic.commandError(tag, exitCode, stdout, stderr));
      if (actionCall.busy) return; // another action follows
      if (root.opened) {
        root.pendingAction = "refreshing";
        root.refresh();
      } else {
        root.pendingAction = "";
      }
    }
  }
}
