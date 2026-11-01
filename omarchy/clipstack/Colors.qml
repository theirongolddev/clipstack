import QtQuick
import qs.Commons as Commons

// Every colour the overlay draws, mapped from the live omarchy theme
// (Commons.Color, which the shell reloads on `omarchy theme set`, so the
// bindings below follow a theme switch without a restart). The card uses the
// theme's [menu] surface, as the built-in clipboard and emoji pickers do; the
// rest is the theme's text, background, accent and urgent colours, and tints
// mixed from them, so the same layering reads on light and dark themes.
// Nothing else in the plugin names a colour.
QtObject {
  id: c

  // Themes set sizes, not families (those follow `omarchy font set` for the
  // shell's monospace alias); the overlay keeps its own pair.
  readonly property string sans: "Noto Sans"
  readonly property string mono: "JetBrains Mono"

  // ---------------------------------------------------------- theme inputs
  // `menu` holds the [menu] tokens (as a var: its properties are dynamic)
  readonly property var menu: Commons.Color.menu
  readonly property color bg: c.opaque(c.menu.background)
  readonly property color fg: c.opaque(c.menu.text)
  readonly property color themeAccent: c.opaque(Commons.Color.accent)
  readonly property color urgent: c.opaque(Commons.Color.urgent)

  function opaque(x) {
    return Qt.rgba(x.r, x.g, x.b, 1);
  }

  function alpha(x, a) {
    return Qt.rgba(x.r, x.g, x.b, a);
  }

  // `a` moved a fraction `t` of the way to `b`, opaque (rich text and the
  // search marks need flat colours)
  function mix(a, b, t) {
    return Qt.rgba(a.r + (b.r - a.r) * t, a.g + (b.g - a.g) * t, a.b + (b.b - a.b) * t, 1);
  }

  function luminance(x) {
    function lin(v) {
      return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
    }
    return 0.2126 * lin(x.r) + 0.7152 * lin(x.g) + 0.0722 * lin(x.b);
  }

  function contrast(a, b) {
    var la = c.luminance(a);
    var lb = c.luminance(b);
    return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
  }

  // `x`, moved toward the theme's text colour until it reaches `ratio`
  // against `against` (a dark accent on a dark theme stays legible as text)
  function readable(x, against, ratio) {
    for (let t = 0; t < 1; t += 0.1) {
      const y = c.mix(x, c.fg, t);
      if (c.contrast(y, against) >= ratio) return y;
    }
    return c.fg;
  }

  // ---------------------------------------------------------- roles
  readonly property color accent: c.themeAccent
  // The accent as text or a thin glyph on the panel
  readonly property color accentText: c.readable(c.themeAccent, c.listPane, 4.5)
  // Text on an accent fill (Paste): background or text, whichever reads better
  readonly property color accentInk: c.contrast(c.bg, c.themeAccent) >= c.contrast(c.fg, c.themeAccent) ? c.bg : c.fg

  readonly property color scrim: c.menu.scrim
  readonly property color card: c.menu.background
  readonly property color cardBorder: c.menu.border
  readonly property color listPane: c.mix(c.bg, c.fg, 0.03)
  readonly property color paneDivider: c.alpha(c.fg, 0.08)
  readonly property color footerDivider: c.alpha(c.fg, 0.06)

  readonly property color searchBox: c.mix(c.bg, c.fg, 0.07)
  readonly property color searchRing: c.alpha(c.themeAccent, 0.6)
  readonly property color searchText: c.fg
  readonly property color placeholder: c.mix(c.bg, c.fg, 0.5)

  readonly property color group: c.mix(c.bg, c.fg, 0.5)
  readonly property color rowSelected: c.mix(c.listPane, c.themeAccent, 0.26)
  readonly property color rowText: c.mix(c.bg, c.fg, 0.82)
  readonly property color rowTextSelected: c.fg
  readonly property color muted: c.mix(c.bg, c.fg, 0.55)
  readonly property color ageSelected: c.mix(c.rowSelected, c.fg, 0.7)
  readonly property color chip: c.mix(c.listPane, c.themeAccent, 0.14)
  readonly property color chipPending: c.mix(c.listPane, c.accentText, 0.6)
  // Query marks: accent at ~30% flattened over each background they sit on
  readonly property color mark: c.mix(c.listPane, c.themeAccent, 0.3)
  readonly property color markSelected: c.mix(c.rowSelected, c.themeAccent, 0.3)
  readonly property color markCode: c.mix(c.code, c.themeAccent, 0.3)
  readonly property color markCodeLine: c.mix(c.mix(c.code, c.themeAccent, 0.12), c.themeAccent, 0.3)

  readonly property color kindChip: c.mix(c.bg, c.themeAccent, 0.2)
  readonly property color kindText: c.readable(c.themeAccent, c.kindChip, 4.5)
  readonly property color metaText: c.mix(c.bg, c.fg, 0.62)
  readonly property color metaDot: c.mix(c.bg, c.fg, 0.3)

  readonly property color code: c.mix(c.bg, c.fg, 0.03)
  readonly property color codeBorder: c.alpha(c.fg, 0.06)
  readonly property color codeText: c.mix(c.bg, c.fg, 0.93)
  readonly property color lineNumber: c.mix(c.bg, c.fg, 0.32)
  readonly property color lineHighlight: c.alpha(c.themeAccent, 0.12)
  // Code tint: keywords in the accent, literals in its complementary hue
  // (the urgent colour when the accent is grey)
  readonly property color keyword: c.readable(c.themeAccent, c.code, 4.5)
  readonly property color literal: c.readable(c.themeAccent.hslSaturation < 0.15 ? c.urgent
    : Qt.hsla((c.themeAccent.hslHue + 0.5) % 1, Math.max(c.themeAccent.hslSaturation, 0.5),
      c.themeAccent.hslLightness, 1), c.code, 4.5)

  readonly property color checkerA: c.mix(c.bg, c.fg, 0.04)
  readonly property color checkerB: c.mix(c.bg, c.fg, 0.015)
  readonly property color badge: c.alpha(c.bg, 0.85)
  readonly property color thumbA: c.mix(c.bg, c.themeAccent, 0.3)
  readonly property color thumbB: c.mix(c.bg, c.themeAccent, 0.15)

  readonly property color button: c.mix(c.bg, c.fg, 0.08)
  readonly property color buttonText: c.mix(c.bg, c.fg, 0.88)
  readonly property color buttonHover: c.fg
  readonly property color danger: c.readable(c.urgent, c.bg, 4.5)
  readonly property color dangerKey: c.mix(c.bg, c.danger, 0.7)

  readonly property color prompt: c.mix(c.bg, c.themeAccent, 0.08)
  readonly property color promptRing: c.alpha(c.themeAccent, 0.4)
  readonly property color promptTitle: c.mix(c.bg, c.fg, 0.92)
  readonly property color promptKey: c.readable(c.themeAccent, c.prompt, 4.5)
  // The footer dot and "available" line when all is well
  readonly property color ok: c.readable(c.themeAccent, c.prompt, 4.5)
}
