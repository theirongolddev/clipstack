import QtQuick
import QtQuick.Effects

// The image preview: the full PNG fitted to the pane (never upscaled) on a
// checkerboard, decoded at the size it is shown.
Item {
  id: box

  required property Store ov
  property var entry: null

  readonly property Colors c: box.ov.c
  readonly property string url: box.entry ? box.ov.imageUrl(box.entry.id) : ""
  readonly property real naturalW: box.entry && box.entry.width > 0 ? box.entry.width : 1
  readonly property real naturalH: box.entry && box.entry.height > 0 ? box.entry.height : 1
  readonly property real fit: Math.min(1, (box.width - 48) / box.naturalW, (box.height - 48) / box.naturalH)

  Canvas {
    id: checker
    anchors.fill: parent
    onWidthChanged: requestPaint()
    onHeightChanged: requestPaint()
    onPaint: {
      var ctx = getContext("2d");
      ctx.reset();
      ctx.beginPath();
      ctx.roundedRect(0, 0, width, height, 14, 14);
      ctx.clip();
      ctx.fillStyle = String(box.c.checkerB);
      ctx.fillRect(0, 0, width, height);
      ctx.fillStyle = String(box.c.checkerA);
      for (let y = 0; y < height; y += 9) {
        for (let x = (y / 9) % 2 === 0 ? 0 : 9; x < width; x += 18) ctx.fillRect(x, y, 9, 9);
      }
    }
  }

  Rectangle {
    anchors.fill: parent
    radius: 14
    color: "transparent"
    border.width: 1
    border.color: box.c.codeBorder
  }

  RectangularShadow {
    anchors.fill: picture
    visible: picture.status === Image.Ready
    offset.y: 16
    blur: 50
    radius: 4
    color: Qt.rgba(0, 0, 0, 0.55)
  }

  Image {
    id: picture
    anchors.centerIn: parent
    width: Math.round(box.naturalW * box.fit)
    height: Math.round(box.naturalH * box.fit)
    source: box.url
    sourceSize: Qt.size(Math.ceil(picture.width * Screen.devicePixelRatio),
                        Math.ceil(picture.height * Screen.devicePixelRatio))
    fillMode: Image.Stretch
    asynchronous: true
    smooth: true
    mipmap: true
  }

  Text {
    anchors.centerIn: parent
    visible: box.entry !== null && (box.ov.imagePaths[box.entry.id] === "" || picture.status === Image.Error)
    text: "Image unavailable"
    textFormat: Text.PlainText
    font.family: box.c.sans
    font.pixelSize: 13
    color: box.c.metaText
  }

  Rectangle {
    visible: picture.status === Image.Ready
    anchors.right: parent.right
    anchors.bottom: parent.bottom
    anchors.rightMargin: 14
    anchors.bottomMargin: 12
    width: badge.implicitWidth + 16
    height: badge.implicitHeight + 6
    radius: 6
    color: box.c.badge

    Text {
      id: badge
      anchors.centerIn: parent
      text: box.fit < 1 ? "fit to pane" : "actual size"
      textFormat: Text.PlainText
      font.family: box.c.sans
      font.pixelSize: 11
      color: box.c.metaText
    }
  }
}
