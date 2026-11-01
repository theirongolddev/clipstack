import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons as Commons
import "plugin"

// Headless render of the four design states (C1 browse, C2 search, C3
// image, C4 alias) against tests/fake-clipstack, for comparing with the
// boards. scripts/test-overlay.sh --render copies the plugin and omarchy's
// Commons next to this file (Quickshell only imports from inside its config
// dir) and runs it with QT_QPA_PLATFORM=offscreen; it never touches the live
// shell. The theme is whatever Commons loaded from the (temp) HOME; with
// RENDER_SWITCH_THEME set, it then loads that theme dir the way the shell's
// applyTheme IPC does and draws C1 again: the overlay follows a theme switch
// without a restart.
ShellRoot {
  id: shellRoot

  readonly property string outDir: Quickshell.env("RENDER_OUT")
  readonly property string switchTheme: Quickshell.env("RENDER_SWITCH_THEME") || ""
  property int step: 0

  FileView {
    id: switchColors
    path: shellRoot.switchTheme !== "" ? shellRoot.switchTheme + "/colors.toml" : ""
    blockLoading: true
    printErrors: false
  }

  FileView {
    id: switchShell
    path: shellRoot.switchTheme !== "" ? shellRoot.switchTheme + "/shell.toml" : ""
    blockLoading: true
    printErrors: false
  }

  Store {
    id: store
  }

  FloatingWindow {
    implicitWidth: 1200
    implicitHeight: 760
    color: "#0E0E10"

    Item {
      id: scene
      anchors.fill: parent

      Rectangle {
        anchors.fill: parent
        color: "#0E0E10"
      }
      Rectangle {
        x: 60
        y: 50
        width: 520
        height: 660
        radius: 12
        color: "#19191C"
      }
      Rectangle {
        x: 620
        y: 50
        width: 520
        height: 300
        radius: 12
        color: "#17171A"
      }
      Rectangle {
        anchors.fill: parent
        color: store.c.scrim
      }
      Reader {
        x: 80
        y: 70
        ov: store
      }
    }
  }

  function selectId(id) {
    for (let i = 0; i < store.rows.length; i++) {
      if (store.rows[i].id === id) store.select(i);
    }
  }

  function grab(name) {
    scene.grabToImage(function (result) {
      result.saveToFile(shellRoot.outDir + "/" + name + ".png");
      console.log("rendered " + name);
    });
  }

  // Each step waits for the fake CLI (a few hundred ms) before grabbing
  Timer {
    interval: 500
    repeat: true
    running: true
    onTriggered: {
      shellRoot.step += 1;
      switch (shellRoot.step) {
      case 1:
        store.open("{}");
        break;
      case 2:
        shellRoot.selectId(3);
        break;
      case 3:
        shellRoot.grab("C1-browse");
        break;
      case 4:
        store.setQuery("dock");
        break;
      case 5:
        shellRoot.selectId(4);
        break;
      case 6:
        shellRoot.grab("C2-search");
        break;
      case 7:
        store.setQuery("");
        break;
      case 8:
        shellRoot.selectId(6);
        break;
      case 9:
        shellRoot.grab("C3-image");
        break;
      case 10:
        shellRoot.selectId(3);
        break;
      case 11:
        store.act("alias");
        store.aliasInput = "dcu";
        break;
      case 12:
        shellRoot.grab("C4-alias");
        break;
      case 13:
        store.aliasInput = "sig";
        break;
      case 14:
        shellRoot.grab("C4-alias-taken");
        break;
      case 15:
        store.cancelAlias();
        if (shellRoot.switchTheme === "") {
          Qt.quit();
          break;
        }
        Commons.Color.loadColors(switchColors.text());
        Commons.Color.loadShell(switchShell.loaded ? switchShell.text() : "");
        break;
      case 16:
        shellRoot.grab("C1-browse-switched");
        break;
      case 17:
        Qt.quit();
        break;
      }
    }
  }
}
