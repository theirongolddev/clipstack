pragma ComponentBehavior: Bound

import QtQuick
import "Logic.js" as Logic

// The text preview: line-number gutter, JetBrains Mono, at most the first
// 2000 lines. While searching, matches are marked and the first line with a
// match is highlighted and scrolled into view.
Rectangle {
  id: box

  required property Store ov
  property string text: ""

  readonly property Colors c: box.ov.c
  readonly property var preview: Logic.previewLines(box.text, 2000)
  readonly property int hitLine: box.ov.searching ? Logic.firstMatchLine(box.preview.lines, box.ov.query) : -1
  readonly property int fontSize: box.preview.lines.length <= 8 ? 15 : 13
  readonly property int lineHeight: Math.round(box.fontSize * 1.65)
  // 44 px of right-aligned numbers plus 16 px before the text (board C1)
  readonly property int gutter: 60
  // A light syntax tint for entries that look like code
  readonly property bool tint: Logic.looksLikeCode(box.text)

  FontMetrics {
    id: metrics
    font.family: box.c.mono
    font.pixelSize: box.fontSize
  }

  radius: 14
  color: box.c.code
  border.width: 1
  border.color: box.c.codeBorder
  clip: true

  ListView {
    id: lines
    anchors.fill: parent
    anchors.margins: 1
    topMargin: 15
    bottomMargin: 15
    clip: true
    boundsBehavior: Flickable.StopAtBounds
    model: box.preview.lines
    // Long single lines are cut for layout speed; the entry itself is whole
    readonly property int maxChars: 4000

    delegate: Item {
      id: line
      required property string modelData
      required property int index
      readonly property bool hit: line.index === box.hitLine
      readonly property string shown: line.modelData.length > lines.maxChars
        ? line.modelData.slice(0, lines.maxChars) + "…" : line.modelData
      readonly property bool marked: box.ov.searching
        && Logic.highlightSpans(line.shown, box.ov.query).some(function (s) { return s.match; })
      readonly property bool rich: line.marked || (box.tint && line.shown.trim() !== "")
      // Centre one text line in the 1.65 line box; wrapped lines keep the
      // font's own spacing
      readonly property real inset: Math.max(0, (box.lineHeight - metrics.height) / 2)

      width: ListView.view.width
      height: Math.max(box.lineHeight, code.contentHeight + 2 * line.inset)

      Rectangle {
        anchors.fill: parent
        visible: line.hit
        color: box.c.lineHighlight
        Rectangle {
          width: 3
          height: parent.height
          color: box.c.accent
        }
      }

      Text {
        id: number
        y: line.inset
        width: box.gutter
        rightPadding: 16
        horizontalAlignment: Text.AlignRight
        text: String(line.index + 1)
        textFormat: Text.PlainText
        font.family: box.c.mono
        font.pixelSize: box.fontSize
        color: line.hit ? box.c.accent : box.c.lineNumber
      }

      Text {
        id: code
        x: box.gutter
        y: line.inset
        width: parent.width - box.gutter - 16
        wrapMode: Text.WrapAtWordBoundaryOrAnywhere
        textFormat: line.rich ? Text.RichText : Text.PlainText
        text: line.rich
          ? "<span style=\"white-space:pre-wrap\">" + Logic.codeHtml(line.shown, box.ov.query, box.tint, {
              keyword: box.c.accent,
              literal: box.c.literal,
              mark: line.hit ? box.c.markCodeLine : box.c.markCode,
              markText: box.c.searchText
            }) + "</span>"
          : line.shown
        font.family: box.c.mono
        font.pixelSize: box.fontSize
        color: box.c.codeText
      }
    }

    footer: Text {
      visible: box.preview.truncated
      width: lines.width
      height: visible ? implicitHeight + 8 : 0
      leftPadding: box.gutter
      topPadding: 8
      text: "… " + (box.preview.total - box.preview.lines.length) + " more lines not shown"
      textFormat: Text.PlainText
      font.family: box.c.sans
      font.pixelSize: 12
      color: box.c.metaText
    }

    function follow() {
      if (box.hitLine >= 0) lines.positionViewAtIndex(box.hitLine, ListView.Contain);
      else lines.positionViewAtBeginning();
    }
  }

  onHitLineChanged: Qt.callLater(lines.follow)
  onTextChanged: Qt.callLater(lines.follow)
}
