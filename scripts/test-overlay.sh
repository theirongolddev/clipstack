#!/usr/bin/env bash
# Checks the omarchy-shell overlay plugin (omarchy/clipstack):
#   1. node unit tests for Logic.js (omarchy/clipstack/tests/*.test.mjs)
#   2. qmllint on every QML file, failing on any warning
#   3. omarchy plugin validate on the manifest (skipped without omarchy)
#   4. scripts/install-omarchy-plugin.sh against fake omarchy commands
#      (tests/install-plugin.test.sh)
#   5. headless interaction checks of Store.qml/CliCall.qml
#      (tests/behaviour/shell.qml against tests/fake-clipstack)
#
# --render [DIR] also draws the four design states (browse, search, image,
# alias) headless with QT_QPA_PLATFORM=offscreen against
# tests/fake-clipstack, once per theme, into DIR/<theme>/ (default DIR: a
# new temp dir), plus DIR/<first theme>/C1-browse-switched.png after a live
# switch to the second theme. Themes: OVERLAY_THEMES (theme dirs, space
# separated), else the current omarchy theme and flexoki-light. It fails on
# any QML warning printed at runtime.
#
# The overlay's colours come from omarchy's Commons (OMARCHY_PATH, default
# /usr/share/omarchy). The harnesses load a copy of it in a temp HOME whose
# current theme is a copy of the theme under test (read, never changed), so
# nothing here changes the user's theme, talks to the running omarchy-shell, the
# real clipstack binary or the real history.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
plugin=$root/omarchy/clipstack
render=0
render_dir=
while (($#)); do
  case $1 in
    --render)
      render=1
      if [[ ${2:-} != "" && ${2:0:1} != "-" ]]; then render_dir=$2; shift; fi
      ;;
    -h|--help)
      sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

omarchy=${OMARCHY_PATH:-/usr/share/omarchy}
commons=$omarchy/shell/Commons
[[ -f $commons/qmldir ]] || { echo "omarchy's shell Commons not found at $commons (set OMARCHY_PATH)" >&2; exit 1; }
work=$(mktemp -d)
trap 'rm -r "$work"' EXIT
# `import qs.Commons` resolves against <import dir>/qs/Commons
mkdir -p "$work/imports/qs"
ln -s "$commons" "$work/imports/qs/Commons"

failed=0
step() { printf '\n== %s\n' "$1"; }
fail() { echo "FAIL: $1" >&2; failed=1; }

step "Logic.js unit tests (node)"
command -v node >/dev/null || { echo "node is required" >&2; exit 1; }
node --test "$plugin"/tests/*.test.mjs || fail "node tests"

step "qmllint"
qmllint=${QMLLINT:-}
if [[ -z $qmllint ]]; then
  if [[ -x /usr/lib/qt6/bin/qmllint ]]; then qmllint=/usr/lib/qt6/bin/qmllint; else qmllint=$(command -v qmllint || true); fi
fi
if [[ -z $qmllint ]]; then
  fail "qmllint not found (set QMLLINT)"
elif "$qmllint" -I "$work/imports" --max-warnings 0 "$plugin"/*.qml; then
  echo "ok: $(ls "$plugin"/*.qml | wc -l) files, no warnings"
else
  fail "qmllint"
fi

step "omarchy plugin validate"
if command -v omarchy >/dev/null; then
  if omarchy plugin validate "$plugin"; then echo "ok"; else fail "omarchy plugin validate"; fi
else
  echo "skipped: omarchy is not installed"
fi

step "install script (fake omarchy)"
"$plugin/tests/install-plugin.test.sh" || fail "install script tests"

# The current omarchy theme, then flexoki-light (a light one), unless
# OVERLAY_THEMES names theme dirs
themes=()
if [[ -n ${OVERLAY_THEMES:-} ]]; then
  read -ra themes <<<"$OVERLAY_THEMES"
else
  current=$HOME/.local/state/omarchy/current/theme
  if [[ -f $current/colors.toml ]]; then themes+=("$current"); fi
  if [[ $(cat "$current/../theme.name" 2>/dev/null || true) != flexoki-light ]]; then
    themes+=("$omarchy/themes/flexoki-light")
  fi
fi
theme_name() {
  if [[ $(basename "$1") == theme && -f $1/../theme.name ]]; then cat "$1/../theme.name"; else basename "$1"; fi
}

# Copies the plugin, omarchy's Commons (as the config's qs.Commons) and a
# harness shell.qml into a fresh Quickshell config (it only imports from
# inside its config dir) and runs it headless against a fresh fake history,
# in a temp HOME whose current omarchy theme is a copy of $2. Prints the log
# path; extra env goes before the call.
run_harness() {
  local harness=$1 theme=$2 name=$3 dir
  # An odd name, so file paths reach Quickshell with a space, # and %
  dir=$(mktemp -d "$work/$name #1 %20.XXXX")
  mkdir -p "$dir/config/plugin" "$dir/home/.local/state/omarchy/current/theme"
  cp "$plugin"/*.qml "$plugin"/Logic.js "$dir/config/plugin/"
  cp -r "$commons" "$dir/config/Commons"
  cp "$harness" "$dir/config/shell.qml"
  if [[ -n $theme ]]; then
    cp "$theme"/colors.toml "$dir/home/.local/state/omarchy/current/theme/"
    if [[ -f $theme/shell.toml ]]; then cp "$theme"/shell.toml "$dir/home/.local/state/omarchy/current/theme/"; fi
  fi
  # Seed the fake history first (it draws its PNG on first use)
  CLIPSTACK_FAKE_DIR=$dir/fake "$plugin/tests/fake-clipstack" list --json >/dev/null
  # Fonts still come from the real fontconfig setup
  HOME=$dir/home XDG_CONFIG_HOME=${XDG_CONFIG_HOME:-$HOME/.config} \
    XDG_DATA_HOME=${XDG_DATA_HOME:-$HOME/.local/share} XDG_CACHE_HOME=${XDG_CACHE_HOME:-$HOME/.cache} \
    CLIPSTACK_BIN=$plugin/tests/fake-clipstack CLIPSTACK_FAKE_DIR=$dir/fake \
    QT_QPA_PLATFORM=offscreen timeout 60 quickshell -p "$dir/config" >"$dir/quickshell.log" 2>&1 || true
  echo "$dir/quickshell.log"
}

# Offscreen-platform notices (and the start failure the interaction checks
# cause on purpose) are expected; any other warning is a bug
qml_warnings() {
  grep -E 'WARN|ERROR' "$1" |
    grep -vE 'QT_QPA_PLATFORM|WAYLAND_DISPLAY|--- WARNING ---|actually running wayland|window masks|QList\("/nonexistent/'
}

if command -v quickshell >/dev/null; then
  step "interaction checks (fake clipstack)"
  log=$(run_harness "$plugin/tests/behaviour/shell.qml" "${themes[0]:-}" behaviour)
  grep -o 'CHECK .*' "$log" || true
  if qml_warnings "$log"; then fail "QML warnings at runtime (log above)"; fi
  grep -q 'CHECK FAIL' "$log" && fail "interaction checks"
  grep -q 'CHECKS DONE' "$log" || { cat "$log"; fail "interaction checks did not finish"; }
else
  fail "quickshell not found (interaction checks)"
fi

if ((render)); then
  step "headless render (fake clipstack)"
  [[ -n $render_dir ]] || render_dir=$(mktemp -d)
  mkdir -p "$render_dir"
  ((${#themes[@]})) || fail "no theme to render (set OVERLAY_THEMES)"
  for i in "${!themes[@]}"; do
    theme=${themes[i]}
    out=$render_dir/$(theme_name "$theme")
    mkdir -p "$out"
    # The first run also switches to the second theme live, the way
    # `omarchy theme set` reaches the shell (its applyTheme IPC)
    switch=
    if ((i == 0)); then switch=${themes[1]:-}; fi
    log=$(RENDER_OUT=$out RENDER_SWITCH_THEME=$switch run_harness "$plugin/tests/render/shell.qml" "$theme" render)
    if qml_warnings "$log"; then fail "QML warnings at runtime (log above)"; fi
    for name in C1-browse C2-search C3-image C4-alias C4-alias-taken; do
      [[ -s $out/$name.png ]] || fail "no $(theme_name "$theme")/$name.png"
    done
    if [[ -n $switch ]]; then
      if [[ ! -s $out/C1-browse-switched.png ]]; then
        fail "no C1-browse-switched.png"
      elif cmp -s "$out/C1-browse.png" "$out/C1-browse-switched.png"; then
        fail "the overlay did not change colour on a live theme switch"
      fi
    fi
    echo "rendered $(theme_name "$theme") into $out"
  done
fi

if ((failed)); then
  echo; echo "overlay checks FAILED" >&2
  exit 1
fi
echo; echo "overlay checks passed"
