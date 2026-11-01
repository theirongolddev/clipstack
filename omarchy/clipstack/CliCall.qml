import QtQuick
import Quickshell.Io

// One argv command at a time through Quickshell.Io.Process, reporting
// finished(tag, exitCode, stdout, stderr) once the process has exited and
// both streams are read. A command that cannot start reports exit 127.
//
// latestOnly (lists, searches, previews): a run() while busy replaces any
// queued run, and the busy run's result is dropped as stale. Otherwise
// (actions, image paths) runs queue and every one reports.
Item {
  id: call

  property bool latestOnly: true
  readonly property bool busy: job !== null

  property var job: null
  property var queue: []
  property bool exited: false
  property bool outDone: false
  property bool errDone: false
  property int exitCode: 0

  signal finished(var tag, int exitCode, string stdout, string stderr)

  function run(argv, tag) {
    var next = { argv: argv, tag: tag };
    if (call.job !== null) {
      call.queue = call.latestOnly ? [next] : call.queue.concat([next]);
      return;
    }
    call.start(next);
  }

  function start(next) {
    startFailTimer.stop();
    call.job = next;
    call.exited = false;
    call.outDone = false;
    call.errDone = false;
    call.exitCode = 0;
    proc.command = next.argv;
    proc.running = true;
  }

  // Starts the job deliver() reserved, outside the Process's own signals
  function launch() {
    if (call.job !== null && !call.exited && !proc.running) call.start(call.job);
  }

  function settle() {
    if (call.job === null || !call.exited) return;
    if (!(call.outDone && call.errDone)) {
      settleTimer.restart();
      return;
    }
    call.deliver();
  }

  function deliver() {
    settleTimer.stop();
    startFailTimer.stop();
    var done = { tag: call.job.tag, exitCode: call.exitCode };
    var stdout = out.text;
    var stderr = err.text;
    var stale = call.latestOnly && call.queue.length > 0;
    // The next queued run is reserved as the job before anyone hears about
    // this one, so a run() from a finished handler queues behind it instead
    // of starting a second command on the same Process
    if (call.queue.length > 0) {
      call.job = call.queue[0];
      call.queue = call.queue.slice(1);
      Qt.callLater(call.launch);
    } else {
      call.job = null;
    }
    call.exited = false;
    if (!stale) call.finished(done.tag, done.exitCode, stdout, stderr);
  }

  Process {
    id: proc

    stdout: StdioCollector {
      id: out
      onStreamFinished: {
        call.outDone = true;
        call.settle();
      }
    }
    stderr: StdioCollector {
      id: err
      onStreamFinished: {
        call.errDone = true;
        call.settle();
      }
    }

    onExited: function (exitCode) {
      call.exitCode = exitCode;
      call.exited = true;
      call.settle();
    }
    onRunningChanged: {
      if (!proc.running && call.job !== null && !call.exited) startFailTimer.restart();
    }
  }

  // Not running and never exited: the command could not be started.
  Timer {
    id: startFailTimer
    interval: 300
    onTriggered: {
      if (call.job === null || call.exited || proc.running) return;
      call.exitCode = 127;
      call.exited = true;
      call.outDone = true;
      call.errDone = true;
      call.deliver();
    }
  }

  // Exited but a stream never reported its end: deliver what was read.
  Timer {
    id: settleTimer
    interval: 250
    onTriggered: {
      call.outDone = true;
      call.errDone = true;
      call.deliver();
    }
  }
}
