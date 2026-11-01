import QtQuick
import QtQuick.Effects

// The 34x22 rounded thumbnail of an image row. The PNG is decoded at twice
// the box size, never at full resolution.
Item {
  id: thumb

  required property Colors c
  property string source: ""

  implicitWidth: 34
  implicitHeight: 22

  Rectangle {
    anchors.fill: parent
    radius: 4
    gradient: Gradient {
      orientation: Gradient.Horizontal
      GradientStop { position: 0; color: thumb.c.thumbA }
      GradientStop { position: 1; color: thumb.c.thumbB }
    }
  }

  Image {
    id: image
    anchors.fill: parent
    source: thumb.source
    sourceSize: Qt.size(68, 44)
    fillMode: Image.PreserveAspectCrop
    asynchronous: true
    smooth: true
    visible: false
  }

  Rectangle {
    id: mask
    anchors.fill: parent
    radius: 4
    visible: false
    layer.enabled: true
  }

  MultiEffect {
    anchors.fill: parent
    source: image
    visible: image.status === Image.Ready
    maskEnabled: true
    maskSource: mask
  }
}
