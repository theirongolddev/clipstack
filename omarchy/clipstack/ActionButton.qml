import QtQuick

// A preview-pane action with its shortcut shown: "primary" (Paste),
// "normal" or "danger" (Delete).
Rectangle {
  id: button

  required property Colors c
  property string label: ""
  property string keyHint: ""
  property string variant: "normal"
  signal activated()

  readonly property bool primary: button.variant === "primary"
  readonly property bool danger: button.variant === "danger"

  implicitWidth: content.implicitWidth + (button.primary ? 32 : button.danger ? 24 : 28)
  implicitHeight: 40
  radius: 10
  color: button.primary ? button.c.accent : button.danger ? "transparent" : button.c.button
  opacity: hover.pressed ? 0.8 : 1

  Rectangle {
    anchors.fill: parent
    radius: parent.radius
    color: "white"
    opacity: hover.containsMouse ? 0.06 : 0
  }

  Row {
    id: content
    anchors.centerIn: parent
    spacing: button.primary ? 10 : 8

    Text {
      anchors.verticalCenter: parent.verticalCenter
      text: button.label
      textFormat: Text.PlainText
      font.family: button.c.sans
      font.pixelSize: button.primary ? 14 : 13
      font.weight: button.primary ? Font.DemiBold : Font.Normal
      color: button.primary ? button.c.accentInk : button.danger ? button.c.danger : button.c.buttonText
    }

    Text {
      anchors.verticalCenter: parent.verticalCenter
      visible: button.keyHint !== ""
      text: button.keyHint
      textFormat: Text.PlainText
      font.family: button.c.sans
      font.pixelSize: button.primary ? 12 : 11
      color: button.primary ? button.c.accentInk : button.danger ? button.c.dangerKey : button.c.muted
      opacity: button.primary ? 0.7 : 1
    }
  }

  MouseArea {
    id: hover
    anchors.fill: parent
    hoverEnabled: true
    cursorShape: Qt.PointingHandCursor
    onClicked: button.activated()
  }
}
