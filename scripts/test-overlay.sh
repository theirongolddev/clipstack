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
      sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
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
  if omarchy plugin validate "$plugin"; then echo "ok"; else fail "omarchy plugin validate"; fi
else
  echo "skipped: omarchy is not installed"
fi

step "install script (fake omarchy)"
"$plugin/tests/install-plugin.test.sh" || fail "install script tests"

# Copies the plugin and a harness shell.qml into a fresh Quickshell config
# (it only imports from inside its config dir) and runs it headless against
# a fresh fake history. Prints the log path; extra env goes before the call.
run_harness() {
  local harness=$1 name=$2 dir
  # An odd name, so file paths reach Quickshell with a space, # and %
  dir=$(mktemp -d "$work/$name #1 %20.XXXX")
  mkdir -p "$dir/config/plugin"
  cp "$plugin"/*.qml "$plugin"/Logic.js "$dir/config/plugin/"
  cp "$harness" "$dir/config/shell.qml"
  # Seed the fake history first (it draws its PNG on first use)
  CLIPSTACK_FAKE_DIR=$dir/fake "$plugin/tests/fake-clipstack" list --json >/dev/null
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
  work=$(mktemp -d)
  trap 'rm -r "$work"' EXIT

  step "interaction checks (fake clipstack)"
  log=$(run_harness "$plugin/tests/behaviour/shell.qml" behaviour)
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
  log=$(RENDER_OUT=$render_dir run_harness "$plugin/tests/render/shell.qml" render)
  if qml_warnings "$log"; then fail "QML warnings at runtime (log above)"; fi
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
