import QtQuick
import Quickshell
import Quickshell.Io
import "plugin"

// Headless interaction checks for Store.qml and CliCall.qml against
// tests/fake-clipstack: request ordering, selection, reopening, repeated
// actions. scripts/test-overlay.sh copies the plugin next to this file and
// runs it with QT_QPA_PLATFORM=offscreen; it prints one "CHECK ok|FAIL"
// line per check and "CHECKS DONE" at the end. Never the live shell.
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

  // Runs sh -c SCRIPT (with $1 = the fake dir) and hands stdout to then()
  function sh(script, then) {
    shProc.then = then || null;
    shProc.command = ["sh", "-c", script, "sh", shellRoot.fakeDir];
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
