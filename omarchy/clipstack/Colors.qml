import QtQuick

// The concept C "Reader" palette (design boards C1-C4). Fixed colours, not
// the omarchy theme tokens.
QtObject {
  readonly property string sans: "Noto Sans"
  readonly property string mono: "JetBrains Mono"

  readonly property color accent: "#A9B4FF"
  readonly property color accentInk: "#12142A"

  readonly property color scrim: Qt.rgba(6 / 255, 6 / 255, 8 / 255, 0.55)
  readonly property color card: "#16161B"
  readonly property color cardBorder: Qt.rgba(1, 1, 1, 0.07)
  readonly property color listPane: "#121216"
  readonly property color paneDivider: Qt.rgba(1, 1, 1, 0.06)
  readonly property color footerDivider: Qt.rgba(1, 1, 1, 0.05)

  readonly property color searchBox: "#1D1D24"
  readonly property color searchRing: Qt.rgba(169 / 255, 180 / 255, 1, 0.5)
  readonly property color searchText: "#F2F3FA"
  readonly property color placeholder: "#8F8FA0"

  readonly property color group: "#8C8C9C"
  readonly property color rowSelected: "#262A45"
  readonly property color rowText: "#CFCFDA"
  readonly property color rowTextSelected: "#F2F3FA"
  readonly property color muted: "#8F8FA0"
  readonly property color ageSelected: "#B7BCE0"
  readonly property color chip: "#20233A"
  readonly property color chipPending: "#7F86B8"
  // rgba(169,180,255,0.28) flattened over the row, selected row and code
  // backgrounds (rich text has no alpha backgrounds)
  readonly property color mark: "#3C3F57"
  readonly property color markSelected: "#4B5179"
  readonly property color markCode: "#3B3E56"
  readonly property color markCodeLine: "#454965"

  readonly property color kindChip: "#262A45"
  readonly property color kindText: "#C6CDFF"
  readonly property color metaText: "#9C9CAE"
  readonly property color metaDot: "#55556A"

  readonly property color code: "#101014"
  readonly property color codeBorder: Qt.rgba(1, 1, 1, 0.04)
  readonly property color codeText: "#EDEDF5"
  readonly property color lineNumber: "#4E4E62"
  readonly property color lineHighlight: Qt.rgba(169 / 255, 180 / 255, 1, 0.09)
  readonly property color literal: "#E7C38F"

  readonly property color checkerA: "#111116"
  readonly property color checkerB: "#0C0C0F"
  readonly property color badge: Qt.rgba(16 / 255, 16 / 255, 20 / 255, 0.85)

  readonly property color button: "#20202A"
  readonly property color buttonText: "#D8D8E4"
  readonly property color danger: "#E99A9A"
  readonly property color dangerKey: "#A87070"

  readonly property color prompt: "#1B1C2A"
  readonly property color promptRing: Qt.rgba(169 / 255, 180 / 255, 1, 0.3)
  readonly property color promptTitle: "#E8E8F4"
  readonly property color promptKey: "#DCE0FF"
  readonly property color ok: "#7FD49B"
}
