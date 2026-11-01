#!/usr/bin/env bash
# Tests scripts/install-omarchy-plugin.sh against fake `omarchy` and
# `omarchy-shell` commands in a temp HOME. The fakes behave like omarchy's
# own: the shell reads user plugins from $HOME/.config/omarchy/plugins
# (whatever XDG_CONFIG_HOME says), learns a new one only on rescanPlugins,
# and every command needs OMARCHY_PATH. Never touches the real ~/.config or
# the running shell.
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
script=$root/scripts/install-omarchy-plugin.sh
plugin=$root/omarchy/clipstack
id=solsystemlabs.clipstack

work=$(mktemp -d)
trap 'rm -r "$work"' EXIT
fakes=$work/bin
state=$work/state
mkdir -p "$fakes" "$state"

cat >"$fakes/omarchy-shell" <<'EOF'
#!/usr/bin/env bash
[[ -n ${OMARCHY_PATH:-} ]] || { echo "omarchy-shell: OMARCHY_PATH is not set" >&2; exit 1; }
case "$1 $2" in
  "shell rescanPlugins")
    : >"$FAKE_STATE/known"
    for m in "$HOME"/.config/omarchy/plugins/*/manifest.json; do
      [[ -f $m ]] && basename "$(dirname "$m")" >>"$FAKE_STATE/known"
    done
    ;;
  "shell listPlugins")
    jq -Rn '[inputs | {id: .}]' <"$FAKE_STATE/known" ;;
  *) echo "fake omarchy-shell: unexpected $*" >&2; exit 2 ;;
esac
EOF

cat >"$fakes/omarchy" <<'EOF'
#!/usr/bin/env bash
echo "$*" >>"$FAKE_STATE/omarchy.calls"
[[ $1 == plugin ]] || { echo "fake omarchy: unexpected $*" >&2; exit 2; }
touch "$FAKE_STATE/known" "$FAKE_STATE/enabled"
case $2 in
  list) omarchy-shell shell listPlugins ;;
  enable)
    omarchy-shell shell listPlugins >/dev/null || exit 1
    grep -qx "$3" "$FAKE_STATE/known" || {
      echo "omarchy-plugin-enable: plugin '$3' is not known; run: omarchy-shell shell rescanPlugins" >&2; exit 1; }
    echo "$3" >>"$FAKE_STATE/enabled"
    echo "Enabled $3" ;;
  disable)
    omarchy-shell shell listPlugins >/dev/null || exit 1
    grep -vx "$3" "$FAKE_STATE/enabled" >"$FAKE_STATE/enabled.tmp"
    mv "$FAKE_STATE/enabled.tmp" "$FAKE_STATE/enabled"
    echo "Disabled $3" ;;
  *) echo "fake omarchy: unexpected $*" >&2; exit 2 ;;
esac
EOF
chmod +x "$fakes/omarchy" "$fakes/omarchy-shell"

failed=0
check() {
  if eval "$2"; then echo "ok: $1"; else echo "FAIL: $1" >&2; failed=1; fi
}

home=$work/home
dest=$home/.config/omarchy/plugins/$id
mkdir -p "$home"
# A non-session shell: no OMARCHY_PATH, and an XDG_CONFIG_HOME omarchy ignores
install() {
  env -u OMARCHY_PATH HOME="$home" XDG_CONFIG_HOME="$work/xdg" FAKE_STATE="$state" \
    PATH="$fakes:$PATH" "$script" "$@" >"$work/out" 2>&1
}

install; status=$?
cat "$work/out"
check "installs from a non-session shell" '((status == 0))'
check "links into ~/.config/omarchy/plugins, where omarchy-shell looks" '[[ -L $dest && $(readlink "$dest") == "$plugin" ]]'
check "leaves XDG_CONFIG_HOME alone" '[[ ! -e $work/xdg ]]'
check "enables the plugin" 'grep -qx "$id" "$state/enabled"'

install; status=$?
check "installing again is fine" '((status == 0)) && [[ -L $dest ]]'

rm "$dest"
ln -s "$work/gone" "$dest"
install; status=$?
check "replaces a dangling link" '((status == 0)) && [[ $(readlink "$dest") == "$plugin" ]]'

install --uninstall; status=$?
check "uninstall removes the link" '((status == 0)) && [[ ! -e $dest && ! -L $dest ]]'
check "uninstall disables the plugin" '! grep -qx "$id" "$state/enabled"'

mkdir -p "$dest"
install; status=$?
check "refuses to replace a real directory" '((status == 1)) && [[ -d $dest && ! -L $dest ]]'
install --uninstall; status=$?
check "uninstall leaves a real directory alone" '((status == 1)) && [[ -d $dest && ! -L $dest ]]'

((failed == 0)) || { echo "install script tests FAILED" >&2; exit 1; }
echo "install script tests passed"
