import QtQuick
import Quickshell
import Quickshell.Wayland

// omarchy-shell entry point (manifest kind "overlay", keepLoaded). The shell
// calls open(payloadJson), close() and toggle(), and reads `opened`. The
// card sits on the overlay layer with exclusive keyboard focus over a
// dimmed scrim; a click on the scrim closes it.
Item {
  id: root

  // Injected by the shell; unused (the overlay only needs the clipstack CLI)
  property string omarchyPath: ""
  readonly property bool opened: store.opened

  function open(payloadJson) {
    store.open(payloadJson);
  }

  function close() {
    store.close();
  }

  function toggle() {
    store.toggle();
  }

  Store {
    id: store
  }

  // Quickshell's qmltypes stub marks PanelWindow uncreatable; the real
  // type is registered at runtime by its Wayland plugin.
  // qmllint disable uncreatable-type
  PanelWindow {
    // qmllint enable uncreatable-type
    visible: store.opened
    anchors {
      top: true
      bottom: true
      left: true
      right: true
    }
    color: "transparent"
    WlrLayershell.namespace: "clipstack"
    WlrLayershell.layer: WlrLayer.Overlay
    WlrLayershell.keyboardFocus: WlrKeyboardFocus.Exclusive
    exclusionMode: ExclusionMode.Ignore

    Rectangle {
      anchors.fill: parent
      color: store.c.scrim
    }

    MouseArea {
      anchors.fill: parent
      onClicked: store.close()
    }

    Reader {
      anchors.centerIn: parent
      ov: store
    }
  }
}
