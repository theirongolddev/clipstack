import QtQuick
import Quickshell
import Quickshell.Io
import "plugin"

// Headless interaction checks for Store.qml and CliCall.qml against
// tests/fake-clipstack: request ordering, selection, reopening, repeated
// actions. scripts/test-overlay.sh copies the plugin next to this file and
// runs it with QT_QPA_PLATFORM=offscreen; it prints one "CHECK ok|FAIL"
// line per check and "CHECKS DONE" at the end; test-overlay.sh also fails
// on any QML warning, such as rich text trying to load an <img> from clip
// text. Never the live shell.
ShellRoot {
  id: shellRoot

  readonly property string fakeDir: Quickshell.env("CLIPSTACK_FAKE_DIR")
  property int failures: 0
  property var steps: []
  property int stepIndex: 0

  function check(name, ok, detail) {
    if (!ok) shellRoot.failures += 1;
    console.log("CHECK " + (ok ? "ok" : "FAIL") + " " + name + (ok ? "" : ": " + detail));
  }

  function rowIndex(id) {
    for (let i = 0; i < store.rows.length; i++) if (store.rows[i].id === id) return i;
    return -1;
  }

  // Runs sh -c SCRIPT (with $1 = the fake dir, then ARGS) and hands stdout
  // to then()
  function sh(script, then, args) {
    shProc.then = then || null;
    shProc.command = ["sh", "-c", script, "sh", shellRoot.fakeDir].concat(args || []);
    shProc.running = true;
  }

  Process {
    id: shProc
    property var then: null
    stdout: StdioCollector {
      id: shOut
    }
    onExited: {
      const then = shProc.then;
      shProc.then = null;
      if (then) Qt.callLater(function () { then(shOut.text); });
    }
  }

  Store {
    id: store
  }

  // The card on screen for the later steps, so rows and preview lines are
  // really drawn (earlier steps check the Store alone)
  property bool showCard: false
  FloatingWindow {
    implicitWidth: 1040
    implicitHeight: 620

    Loader {
      active: shellRoot.showCard
      sourceComponent: Reader {
        ov: store
      }
    }
  }

  Image {
    id: probe
    source: store.imageUrl(6)
    sourceSize: Qt.size(16, 10)
  }

  // ------------------------------------------------ CliCall ordering
  property var callLog: []
  CliCall {
    id: queued
    latestOnly: false
    onFinished: function (tag, exitCode, stdout, stderr) {
      shellRoot.callLog = shellRoot.callLog.concat([tag + "=" + stdout.trim()]);
      // A run() from inside a finished handler must queue behind B, not
      // replace it
      if (tag === "A") queued.run(["echo", "c"], "C");
    }
  }

  property string missingResult: ""
  CliCall {
    id: missing
    onFinished: function (tag, exitCode, stdout, stderr) {
      shellRoot.missingResult = tag + ":" + exitCode;
    }
  }

  // Each step: [delay before it in ms, function]
  Component.onCompleted: {
    shellRoot.steps = [
      [0, function () {
        queued.run(["echo", "a"], "A");
        queued.run(["echo", "b"], "B");
        missing.run(["/nonexistent/clipstack", "list"], "missing");
      }],
      [800, function () {
        shellRoot.check("CliCall delivers every queued run with its own output",
          shellRoot.callLog.join(",") === "A=a,B=b,C=c", shellRoot.callLog.join(","));
        shellRoot.check("a command that cannot start reports exit 127", shellRoot.missingResult === "missing:127",
          shellRoot.missingResult);
        store.open("{}");
      }],
      [800, function () {
        shellRoot.check("open lists the history", store.rows.length === 9, store.rows.length + " rows");
        shellRoot.sh(": > \"$1/calls.log\"");
      }],
      [200, function () {
        store.setQuery("dock");
      }],
      [800, function () {
        shellRoot.check("typing shows matches", store.rows.length > 0 && store.rows[0].group === "MATCHES",
          JSON.stringify(store.rows.map(function (r) { return r.group; })));
        shellRoot.sh("cat \"$1/calls.log\"", function (log) {
          shellRoot.check("typing runs search only, not list first", /^search /m.test(log) && !/^list /m.test(log),
            JSON.stringify(log));
        });
      }],
      [300, function () {
        store.close();
        store.open("{}");
        const groups = store.rows.map(function (r) { return r.group; });
        shellRoot.check("reopening never shows the last session's matches", groups.indexOf("MATCHES") < 0,
          JSON.stringify(groups));
      }],
      [800, function () {
        shellRoot.sh(": > \"$1/calls.log\"");
        store.select(shellRoot.rowIndex(3));
        // Key repeat: the second press comes before the refresh
        store.act("delete");
        store.act("delete");
      }],
      [1000, function () {
        shellRoot.check("a repeated delete does not fail on the deleted entry", store.status === "", store.status);
        shellRoot.check("the deleted entry is gone", shellRoot.rowIndex(3) < 0, "id 3 still listed");
        shellRoot.check("the selection moves to a neighbour", store.selected !== null, "nothing selected");
        shellRoot.sh("cat \"$1/calls.log\"", function (log) {
          const deletes = log.split("\n").filter(function (l) { return /^delete /.test(l); }).length;
          shellRoot.check("one delete per entry", deletes === 1, deletes + " deletes");
        });
      }],
      [300, function () {
        shellRoot.sh("printf get > \"$1/fail\"");
      }],
      [200, function () {
        store.select(shellRoot.rowIndex(6));
      }],
      [800, function () {
        shellRoot.check("a failed image path shows as unavailable", store.imagePaths[6] === "",
          JSON.stringify(store.imagePaths[6]));
        shellRoot.sh("rm \"$1/fail\"");
      }],
      [200, function () {
        store.close();
        store.open("{}");
      }],
      [800, function () {
        store.select(shellRoot.rowIndex(6));
      }],
      [800, function () {
        shellRoot.check("an image path that failed is tried again on the next open", store.imageUrl(6) !== "",
          JSON.stringify(store.imagePaths[6]));
      }],
      [500, function () {
        // The fake dir's name has a space, # and % in it (test-overlay.sh)
        shellRoot.check("the image loads from its path", probe.status === Image.Ready,
          "status " + probe.status + " for " + probe.source);
        // Clip text that is markup: drawn as rich text, it would load images.
        // A prose first line, so the preview is not tinted as code and its
        // lines turn rich only when the query marks them.
        const img = "<img src=\"injected.png\">";
        const content = "Notes in bold\r\n" + img + " bold <b>x</b> &amp;\n";
        const entry = { id: 10, created_at: 0, last_used: 0, size: content.length, kind: "text",
          preview: img + " bold <b>x</b> &amp;", pinned: false, alias: null };
        shellRoot.sh("printf %s \"$2\" >\"$1/content/10\" && jq --argjson e \"$3\" '. + [$e]' \"$1/entries.json\""
          + " >\"$1/e.tmp\" && mv \"$1/e.tmp\" \"$1/entries.json\"", null, [content, JSON.stringify(entry)]);
      }],
      [300, function () {
        store.close();
        shellRoot.showCard = true;
        store.open("{}");
      }],
      [600, function () {
        store.select(shellRoot.rowIndex(10));
      }],
      [600, function () {
        store.setQuery("bold");
      }],
      [600, function () {
        shellRoot.check("markup in a clip matches as text", store.rows.length === 1 && store.rows[0].id === 10,
          JSON.stringify(store.rows.map(function (r) { return r.id; })));
        store.setQuery("");
      }],
      [600, function () {
        store.select(shellRoot.rowIndex(9));
        shellRoot.sh(": > \"$1/calls.log\"");
      }],
      [200, function () {
        // Enter right after typing, before the search has answered
        store.setQuery("dock");
        store.act("copy");
      }],
      [800, function () {
        shellRoot.sh("cat \"$1/calls.log\"", function (log) {
          shellRoot.check("an action typed ahead of the search acts on its top match",
            /^get id:4 --copy/m.test(log), JSON.stringify(log));
        });
      }],
      [300, function () {
        // A clip pinned while the overlay was closed: the fresh list starts
        // 11, 2 (devbox), 1 (sig); the last one listed 2, 1
        const entry = { id: 11, created_at: Date.now(), last_used: Date.now(), size: 3, kind: "text",
          preview: "new", pinned: true, alias: null };
        shellRoot.sh("printf new >\"$1/content/11\" && jq --argjson e \"$2\" '. + [$e]' \"$1/entries.json\""
          + " >\"$1/e.tmp\" && mv \"$1/e.tmp\" \"$1/entries.json\" && : > \"$1/calls.log\"", null,
          [JSON.stringify(entry)]);
      }],
      [300, function () {
        // Open, Down, Enter before the list has answered
        store.open("{}");
        store.move("down", 10);
        store.act("copy");
      }],
      [800, function () {
        shellRoot.sh("cat \"$1/calls.log\"", function (log) {
          shellRoot.check("keys typed ahead of the list act on the fresh list (second row: id 2)",
            /^get id:2 --copy/m.test(log), JSON.stringify(log));
        });
      }],
    ];
    stepTimer.interval = 1;
    stepTimer.start();
  }

  Timer {
    id: stepTimer
    onTriggered: {
      if (shellRoot.stepIndex >= shellRoot.steps.length) {
        console.log("CHECKS DONE " + shellRoot.failures + " failed");
        Qt.quit();
        return;
      }
      const step = shellRoot.steps[shellRoot.stepIndex];
      shellRoot.stepIndex += 1;
      step[1]();
      const next = shellRoot.steps[shellRoot.stepIndex];
      stepTimer.interval = next ? Math.max(1, next[0]) : 300;
      stepTimer.start();
    }
  }
}
