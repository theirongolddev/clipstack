pragma ComponentBehavior: Bound

import QtQuick

// The inline alias prompt (board C4): live validation against the CLI's
// grammar and the aliases already in the history. Enter saves, Esc cancels.
Rectangle {
  id: prompt

  required property Store ov
  readonly property Colors c: prompt.ov.c
  readonly property var check: prompt.ov.aliasCheck

  implicitHeight: column.implicitHeight + 32
  radius: 14
  color: prompt.c.prompt
  border.width: 1
  border.color: prompt.c.promptRing

  Column {
    id: column
    x: 16
    y: 16
    width: parent.width - 32
    spacing: 12

    Text {
      text: "Alias this clip (pins it)"
      textFormat: Text.PlainText
      font.family: prompt.c.sans
      font.pixelSize: 13
      font.weight: Font.DemiBold
      color: prompt.c.promptTitle
    }

    Rectangle {
      width: parent.width
      height: 46
      radius: 10
      color: prompt.c.code
      border.width: 1
      border.color: prompt.c.accent

      Text {
        id: at
        x: 14
        anchors.verticalCenter: parent.verticalCenter
        text: "@"
        textFormat: Text.PlainText
        font.family: prompt.c.mono
        font.pixelSize: 17
        color: prompt.c.chipPending
      }

      TextInput {
        id: input
        anchors.left: at.right
        anchors.leftMargin: 2
        anchors.right: verdict.left
        anchors.rightMargin: 12
        anchors.verticalCenter: parent.verticalCenter
        font.family: prompt.c.mono
        font.pixelSize: 17
        color: prompt.c.searchText
        selectionColor: prompt.c.rowSelected
        cursorDelegate: Rectangle {
          width: 2
          color: prompt.c.accent
        }
        maximumLength: 40
        clip: true
        onTextEdited: prompt.ov.aliasInput = input.text
        Keys.onPressed: function (event) {
          if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
            prompt.ov.saveAlias();
            event.accepted = true;
          } else if (event.key === Qt.Key_Escape) {
            prompt.ov.cancelAlias();
            event.accepted = true;
          }
        }
      }

      Text {
        id: verdict
        anchors.right: parent.right
        anchors.rightMargin: 14
        anchors.verticalCenter: parent.verticalCenter
        text: prompt.check.message
        textFormat: Text.PlainText
        font.family: prompt.c.sans
        font.pixelSize: 12
        color: prompt.check.state === "available" ? prompt.c.ok
          : prompt.check.state === "current" ? prompt.c.metaText : prompt.c.danger
      }
    }

    Item {
      width: parent.width
      height: hint.implicitHeight

      Text {
        id: hint
        textFormat: Text.StyledText
        text: "Lowercase letters, digits, <font face=\"" + prompt.c.mono + "\" color=\"#CFCFDA\">-</font> and "
          + "<font face=\"" + prompt.c.mono + "\" color=\"#CFCFDA\">_</font>"
        font.family: prompt.c.sans
        font.pixelSize: 12
        color: prompt.c.metaText
      }

      Row {
        anchors.right: parent.right
        spacing: 16

        Text {
          textFormat: Text.StyledText
          text: "<font color=\"" + prompt.c.promptKey + "\">⏎</font> save"
          font.family: prompt.c.sans
          font.pixelSize: 12
          color: prompt.c.metaText
        }

        Text {
          textFormat: Text.StyledText
          text: "<font color=\"" + prompt.c.promptKey + "\">Esc</font> cancel"
          font.family: prompt.c.sans
          font.pixelSize: 12
          color: prompt.c.metaText
        }
      }
    }
  }

  Connections {
    target: prompt.ov
    function onFocusAliasRequested() {
      input.text = prompt.ov.aliasInput;
      input.cursorPosition = input.text.length;
      input.forceActiveFocus();
    }
  }
}
