#!/usr/bin/env bash
# Checks the omarchy-shell overlay plugin (omarchy/clipstack):
#   1. node unit tests for Logic.js (omarchy/clipstack/tests/*.test.mjs)
#   2. qmllint on every QML file, failing on any warning
#   3. omarchy plugin validate on the manifest (skipped without omarchy)
#
# --render [DIR] also draws the four design states (browse, search, image,
# alias) headless with QT_QPA_PLATFORM=offscreen against
# tests/fake-clipstack and saves PNGs in DIR (default: a new temp dir). It
# fails on any QML warning printed at runtime. Nothing here talks to the
# running omarchy-shell, the real clipstack binary or the real history.
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
      sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

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
elif "$qmllint" --max-warnings 0 "$plugin"/*.qml; then
  echo "ok: $(ls "$plugin"/*.qml | wc -l) files, no warnings"
else
  fail "qmllint"
fi

step "omarchy plugin validate"
if command -v omarchy >/dev/null; then
  omarchy plugin validate "$plugin" || fail "omarchy plugin validate"
else
  echo "skipped: omarchy is not installed"
fi

if ((render)); then
  step "headless render (fake clipstack)"
  command -v quickshell >/dev/null || { fail "quickshell not found"; exit 1; }
  work=$(mktemp -d)
  trap 'rm -r "$work"' EXIT
  [[ -n $render_dir ]] || render_dir=$(mktemp -d)
  mkdir -p "$work/config/plugin" "$render_dir"
  cp "$plugin"/*.qml "$plugin"/Logic.js "$work/config/plugin/"
  cp "$plugin/tests/render/shell.qml" "$work/config/shell.qml"
  log=$work/quickshell.log
  # Seed the fake history first (it draws its PNG on first use)
  CLIPSTACK_FAKE_DIR=$work/fake "$plugin/tests/fake-clipstack" list --json >/dev/null
  CLIPSTACK_BIN=$plugin/tests/fake-clipstack CLIPSTACK_FAKE_DIR=$work/fake RENDER_OUT=$render_dir \
    QT_QPA_PLATFORM=offscreen timeout 60 quickshell -p "$work/config" >"$log" 2>&1 || true
  # Offscreen-platform notices are expected; any other warning is a bug
  if grep -E 'WARN|ERROR' "$log" | grep -vE 'QT_QPA_PLATFORM|WAYLAND_DISPLAY|--- WARNING ---|actually running wayland|window masks' ; then
    fail "QML warnings at runtime (log above)"
  fi
  for name in C1-browse C2-search C3-image C4-alias C4-alias-taken; do
    [[ -s $render_dir/$name.png ]] || fail "no $name.png"
  done
  echo "rendered into $render_dir"
fi

if ((failed)); then
  echo; echo "overlay checks FAILED" >&2
  exit 1
fi
echo; echo "overlay checks passed"
