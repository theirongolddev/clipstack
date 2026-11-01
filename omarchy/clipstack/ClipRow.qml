import QtQuick
import QtQuick.Shapes
import "Logic.js" as Logic

// One list row, with its group header (PINNED, TODAY, ...) above it when it
// starts a group. Every state (browse, search, image, alias) uses this row.
Item {
  id: row

  required property Store ov
  required property var modelData
  required property int index

  readonly property Colors c: row.ov.c
  readonly property var entry: row.modelData
  readonly property bool selected: row.index === row.ov.selectedIndex
  readonly property bool first: row.index === 0
  readonly property bool showHeader: row.first || row.ov.rows[row.index - 1].group !== row.entry.group
  readonly property bool image: row.entry.kind === "image"
  readonly property bool mono: !row.image && Logic.looksLikeCode(row.entry.preview)
  readonly property string label: row.image
    ? "Image · " + row.entry.width + "×" + row.entry.height
    : row.entry.preview
  // The alias being typed in the prompt, previewed on the selected row
  readonly property string pendingAlias: row.selected && row.ov.aliasOpen && row.ov.aliasCheck.name !== ""
    && row.ov.aliasCheck.name !== row.entry.alias ? "@" + row.ov.aliasCheck.name : ""
  readonly property bool marked: row.ov.searching && !row.image
    && Logic.highlightSpans(row.label, row.ov.query).some(function (s) { return s.match; })

  width: ListView.view ? ListView.view.width : 0
  height: header.height + body.height

  Text {
    id: header
    visible: row.showHeader
    height: row.showHeader ? implicitHeight : 0
    width: parent.width
    topPadding: row.first ? 8 : 14
    bottomPadding: 4
    leftPadding: 10
    text: row.entry.group
    textFormat: Text.PlainText
    font.family: row.c.sans
    font.pixelSize: 11
    font.weight: Font.DemiBold
    font.letterSpacing: 0.88
    color: row.c.group
  }

  Rectangle {
    id: body
    y: header.height
    width: parent.width
    height: 34
    radius: 10
    color: row.selected ? row.c.rowSelected : "transparent"

    Thumb {
      id: thumb
      visible: row.image
      x: 10
      anchors.verticalCenter: parent.verticalCenter
      source: row.image ? row.ov.imageUrl(row.entry.id) : ""
    }

    TextMetrics {
      id: elider
      font: mainText.font
      text: row.label
      elide: Text.ElideRight
      elideWidth: mainText.width
    }

    Text {
      id: mainText
      anchors.verticalCenter: parent.verticalCenter
      x: row.image ? thumb.x + thumb.width + 10 : 10
      width: tail.x - 10 - x
      textFormat: row.marked ? Text.RichText : Text.PlainText
      text: row.marked
        ? Logic.highlightHtml(elider.elidedText, row.ov.query,
            "<span style=\"background-color:" + (row.selected ? row.c.markSelected : row.c.mark)
              + ";color:" + row.c.rowTextSelected + "\">", "</span>")
        : row.label
      elide: Text.ElideRight
      wrapMode: Text.NoWrap
      maximumLineCount: 1
      clip: row.marked
      font.family: row.mono ? row.c.mono : row.c.sans
      font.pixelSize: row.mono ? 12 : 13
      color: row.selected ? row.c.rowTextSelected : row.c.rowText
    }

    Item {
      id: tail
      anchors.right: parent.right
      anchors.rightMargin: 10
      anchors.verticalCenter: parent.verticalCenter
      width: chip.visible ? chip.width : age.implicitWidth
      height: 20

      Rectangle {
        id: chip
        readonly property string text: row.pendingAlias !== "" ? row.pendingAlias
          : row.entry.alias !== "" ? "@" + row.entry.alias : ""
        visible: chip.text !== ""
        anchors.verticalCenter: parent.verticalCenter
        width: chipText.implicitWidth + 14
        height: chipText.implicitHeight + 4
        radius: 6
        color: row.c.chip

        Shape {
          anchors.fill: parent
          visible: row.pendingAlias !== ""
          preferredRendererType: Shape.CurveRenderer
          ShapePath {
            strokeColor: row.c.chipPending
            strokeWidth: 1
            strokeStyle: ShapePath.DashLine
            dashPattern: [3, 2]
            fillColor: "transparent"
            PathRectangle {
              x: 0.5
              y: 0.5
              width: chip.width - 1
              height: chip.height - 1
              radius: 6
            }
          }
        }

        Text {
          id: chipText
          anchors.centerIn: parent
          text: chip.text
          textFormat: Text.PlainText
          font.family: row.c.mono
          font.pixelSize: 11
          color: row.c.accent
        }
      }

      Text {
        id: age
        visible: !chip.visible
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        text: Logic.compactAge(row.entry.lastUsed, row.ov.now)
        textFormat: Text.PlainText
        font.family: row.c.sans
        font.pixelSize: 12
        color: row.selected ? row.c.ageSelected : row.c.muted
      }
    }

    MouseArea {
      anchors.fill: parent
      cursorShape: Qt.PointingHandCursor
      onClicked: row.ov.select(row.index)
      onDoubleClicked: {
        row.ov.select(row.index);
        row.ov.act("paste");
      }
    }
  }

  Component.onCompleted: {
    if (row.image) row.ov.requestImagePath(row.entry.id);
  }
}
