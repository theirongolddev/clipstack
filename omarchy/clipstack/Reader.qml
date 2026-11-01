pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Effects
import QtQuick.Layouts
import QtQuick.Shapes
import "Logic.js" as Logic

// The 1040x620 card of concept C "Reader": the list on the left, the full
// preview of the selected entry on the right.
Item {
  id: reader

  required property Store ov
  readonly property Colors c: reader.ov.c
  readonly property var entry: reader.ov.selected
  readonly property bool image: reader.entry !== null && reader.entry.kind === "image"
  readonly property bool textReady: reader.entry !== null && !reader.image && reader.ov.previewId === reader.entry.id

  width: 1040
  height: 620

  RectangularShadow {
    anchors.fill: card
    offset.y: 30
    blur: 90
    radius: card.radius
    color: Qt.rgba(0, 0, 0, 0.6)
  }

  Rectangle {
    id: card
    anchors.fill: parent
    radius: 20
    color: reader.c.card

    // Clicks inside the card never reach the scrim
    MouseArea {
      anchors.fill: parent
    }

    // ---------------------------------------------------------- list pane
    Rectangle {
      id: listPane
      x: 1
      y: 1
      // 380 including the divider, as on the boards
      width: 380
      height: parent.height - 2
      topLeftRadius: 19
      bottomLeftRadius: 19
      color: reader.c.listPane

      Rectangle {
        anchors.right: parent.right
        width: 1
        height: parent.height
        color: reader.c.paneDivider
      }

      Rectangle {
        id: searchBox
        x: 16
        y: 16
        width: parent.width - 32
        height: 40
        radius: 12
        color: reader.c.searchBox
        border.width: reader.ov.searching ? 1 : 0
        border.color: reader.c.searchRing

        Item {
          id: glass
          x: 14
          width: 16
          height: 16
          anchors.verticalCenter: parent.verticalCenter

          Shape {
            width: 24
            height: 24
            scale: 16 / 24
            transformOrigin: Item.TopLeft
            preferredRendererType: Shape.CurveRenderer
            ShapePath {
              strokeColor: reader.c.accent
              strokeWidth: 2
              fillColor: "transparent"
              capStyle: ShapePath.RoundCap
              PathAngleArc {
                centerX: 11
                centerY: 11
                radiusX: 7
                radiusY: 7
                startAngle: 0
                sweepAngle: 360
              }
              PathMove {
                x: 20
                y: 20
              }
              PathLine {
                x: 16.5
                y: 16.5
              }
            }
          }
        }

        TextInput {
          id: search
          anchors.left: glass.right
          anchors.leftMargin: 10
          anchors.right: count.left
          anchors.rightMargin: 10
          anchors.verticalCenter: parent.verticalCenter
          font.family: reader.c.sans
          font.pixelSize: 14
          color: reader.c.searchText
          selectionColor: reader.c.rowSelected
          selectByMouse: true
          clip: true
          focus: true
          onTextEdited: reader.ov.setQuery(search.text)

          Keys.onPressed: function (event) {
            var action = Logic.keyAction(event.key, event.modifiers, search.text !== "", event.isAutoRepeat);
            if (action === "") return;
            event.accepted = true;
            switch (action) {
            case "clear":
              search.text = "";
              reader.ov.setQuery("");
              break;
            case "close":
              reader.ov.close();
              break;
            case "up":
            case "down":
            case "pageUp":
            case "pageDown":
            case "home":
            case "end":
              reader.ov.move(action, list.pageSize);
              break;
            default:
              reader.ov.act(action);
            }
          }

          Text {
            anchors.verticalCenter: parent.verticalCenter
            visible: search.text === ""
            text: "Type to search"
            textFormat: Text.PlainText
            font: search.font
            color: reader.c.placeholder
          }
        }

        Text {
          id: count
          anchors.right: parent.right
          anchors.rightMargin: 14
          anchors.verticalCenter: parent.verticalCenter
          visible: reader.ov.loaded
          text: Logic.countLabel(reader.ov.rows.length, reader.ov.totals.total, reader.ov.searching)
          textFormat: Text.PlainText
          font.family: reader.c.sans
          font.pixelSize: 12
          color: reader.ov.searching ? reader.c.accentText : reader.c.muted
        }
      }

      ListView {
        id: list
        x: 10
        y: searchBox.y + searchBox.height + 16
        width: parent.width - 1 - 20
        height: statusBar.y - y
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        model: reader.ov.rows
        readonly property int pageSize: Math.max(1, Math.floor(list.height / 34) - 1)

        delegate: ClipRow {
          ov: reader.ov
        }

        function follow() {
          var i = reader.ov.selectedIndex;
          if (i < 0) return;
          // The first row's group header is part of it, so Beginning shows it
          list.positionViewAtIndex(i, i === 0 ? ListView.Beginning : ListView.Contain);
        }
      }

      Text {
        anchors.horizontalCenter: list.horizontalCenter
        y: list.y + 40
        visible: reader.ov.loaded && reader.ov.rows.length === 0
        text: reader.ov.searching ? "No matches for “" + reader.ov.query.trim() + "”" : "Clipboard is empty"
        textFormat: Text.PlainText
        width: list.width - 40
        horizontalAlignment: Text.AlignHCenter
        elide: Text.ElideRight
        font.family: reader.c.sans
        font.pixelSize: 13
        color: reader.c.muted
      }

      Item {
        id: statusBar
        y: parent.height - height
        width: parent.width - 1
        height: 38

        Rectangle {
          width: parent.width
          height: 1
          color: reader.c.footerDivider
        }

        Rectangle {
          id: dot
          x: 20
          anchors.verticalCenter: parent.verticalCenter
          width: 7
          height: 7
          radius: 4
          color: reader.ov.status !== "" ? reader.c.danger : reader.c.ok
        }

        Text {
          anchors.left: dot.right
          anchors.leftMargin: 8
          anchors.right: parent.right
          anchors.rightMargin: 20
          anchors.verticalCenter: parent.verticalCenter
          text: reader.ov.status !== "" ? reader.ov.status
            : reader.ov.searching ? "Esc clears the search"
            : Logic.footerText(reader.ov.totals.total, reader.ov.totals.pinned)
          textFormat: Text.PlainText
          elide: Text.ElideRight
          font.family: reader.c.sans
          font.pixelSize: 11
          color: reader.ov.status !== "" ? reader.c.danger : reader.c.muted
        }
      }
    }

    // ---------------------------------------------------------- preview pane
    ColumnLayout {
      id: previewPane
      x: listPane.x + listPane.width
      y: 1
      width: parent.width - x - 1
      height: parent.height - 2
      spacing: 0
      visible: reader.entry !== null

      AliasPrompt {
        Layout.fillWidth: true
        Layout.topMargin: 22
        Layout.leftMargin: 28
        Layout.rightMargin: 28
        visible: reader.ov.aliasOpen
        ov: reader.ov
      }

      Row {
        id: meta
        Layout.topMargin: 22
        Layout.leftMargin: 28
        Layout.rightMargin: 28
        Layout.fillWidth: true
        visible: !reader.ov.aliasOpen
        spacing: 10
        readonly property var items: reader.entry
          ? Logic.metaItems(reader.entry, reader.textReady ? reader.ov.previewText : reader.entry.preview, reader.ov.now)
          : []

        Rectangle {
          anchors.verticalCenter: parent.verticalCenter
          width: kind.implicitWidth + 20
          height: kind.implicitHeight + 8
          radius: 7
          color: reader.c.kindChip

          Text {
            id: kind
            anchors.centerIn: parent
            text: reader.entry
              ? Logic.kindLabel(reader.entry, reader.textReady ? reader.ov.previewText : reader.entry.preview) : ""
            textFormat: Text.PlainText
            font.family: reader.c.sans
            font.pixelSize: 12
            font.weight: Font.DemiBold
            color: reader.c.kindText
          }
        }

        Repeater {
          model: meta.items
          delegate: Row {
            id: item
            required property string modelData
            required property int index
            anchors.verticalCenter: parent ? parent.verticalCenter : undefined
            spacing: 10

            Rectangle {
              visible: item.index > 0
              anchors.verticalCenter: parent.verticalCenter
              width: 3
              height: 3
              radius: 2
              color: reader.c.metaDot
            }

            Text {
              anchors.verticalCenter: parent.verticalCenter
              text: item.modelData
              textFormat: Text.PlainText
              font.family: reader.c.sans
              font.pixelSize: 12
              color: reader.c.metaText
            }
          }
        }
      }

      Item {
        Layout.fillWidth: true
        Layout.fillHeight: true
        Layout.margins: 28
        Layout.topMargin: 18
        Layout.bottomMargin: 18

        PreviewText {
          anchors.fill: parent
          visible: !reader.image
          ov: reader.ov
          text: reader.textReady ? reader.ov.previewText : ""
        }

        PreviewImage {
          anchors.fill: parent
          visible: reader.image
          ov: reader.ov
          entry: reader.image ? reader.entry : null
        }
      }

      RowLayout {
        Layout.leftMargin: 28
        Layout.rightMargin: 28
        Layout.bottomMargin: 22
        Layout.fillWidth: true
        visible: !reader.ov.aliasOpen
        spacing: 8

        // Right-justified: Delete set apart on the left, Paste at the far right
        Item {
          Layout.fillWidth: true
        }
        ActionButton {
          c: reader.c
          label: "Delete"
          keyHint: "⌃⌦"
          variant: "danger"
          onActivated: reader.ov.act("delete")
        }
        Item {
          // with the row spacing on both sides, a 24px gap
          implicitWidth: 8
        }
        ActionButton {
          c: reader.c
          visible: !reader.image
          label: "Edit"
          keyHint: "⌃E"
          onActivated: reader.ov.act("edit")
        }
        ActionButton {
          c: reader.c
          label: "Alias…"
          keyHint: "⌃A"
          onActivated: reader.ov.act("alias")
        }
        ActionButton {
          c: reader.c
          label: reader.entry && reader.entry.pinned ? "Unpin" : "Pin"
          keyHint: "⌃P"
          onActivated: reader.ov.act("pin")
        }
        ActionButton {
          c: reader.c
          label: "Copy"
          keyHint: "⇧⏎"
          onActivated: reader.ov.act("copy")
        }
        ActionButton {
          c: reader.c
          label: "Paste"
          keyHint: "⏎"
          variant: "primary"
          onActivated: reader.ov.act("paste")
        }
      }
    }

    Rectangle {
      anchors.fill: parent
      radius: parent.radius
      color: "transparent"
      border.width: 1
      border.color: reader.c.cardBorder
    }
  }

  Connections {
    target: reader.ov
    function onFocusSearchRequested() {
      search.text = reader.ov.query;
      search.forceActiveFocus();
    }
    function onQueryChanged() {
      if (search.text !== reader.ov.query) search.text = reader.ov.query;
    }
    function onSelectedIndexChanged() {
      Qt.callLater(list.follow);
    }
    function onRowsChanged() {
      Qt.callLater(list.follow);
    }
  }
}
