#!/usr/bin/env bash
# Installs the ClipStack overlay into omarchy-shell: links this repo's
# omarchy/clipstack folder to ~/.config/omarchy/plugins/solsystemlabs.clipstack
# and enables the plugin. Edits to the repo take effect on the next shell
# reload. --uninstall disables the plugin and removes the link.
#
# Never replaces anything at the plugin path that is not a symlink.
set -euo pipefail

id=solsystemlabs.clipstack
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
src=$root/omarchy/clipstack
# omarchy-shell reads user plugins from here, whatever XDG_CONFIG_HOME says
plugins=$HOME/.config/omarchy/plugins
dest=$plugins/$id

usage() { sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//'; }

uninstall=0
case ${1:-} in
  "") ;;
  --uninstall) uninstall=1 ;;
  -h|--help) usage; exit 0 ;;
  *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
esac

if ! command -v omarchy >/dev/null; then
  echo "omarchy not found: this installs an omarchy-shell plugin" >&2
  exit 1
fi

# omarchy's plugin commands need OMARCHY_PATH, which only session and login
# shells set; take it from omarchy's own bootstrap (in a subshell, so its
# PATH changes stay out of this script)
if [[ -z ${OMARCHY_PATH:-} ]]; then
  bootstrap=/usr/share/omarchy/default/bash/env-bootstrap
  OMARCHY_PATH=
  [[ -r $bootstrap ]] && OMARCHY_PATH=$(bash -c '. "$1" >/dev/null 2>&1; printf %s "$OMARCHY_PATH"' _ "$bootstrap")
  export OMARCHY_PATH=${OMARCHY_PATH:-/usr/share/omarchy}
fi

if ((uninstall)); then
  omarchy plugin disable "$id" || true
  if [[ -L $dest ]]; then
    rm "$dest"
    echo "removed $dest"
  elif [[ -e $dest ]]; then
    echo "left $dest alone: it is not a symlink" >&2
    exit 1
  fi
  exit 0
fi

if [[ -e $dest && ! -L $dest ]]; then
  echo "$dest exists and is not a symlink; move it away first" >&2
  exit 1
fi

mkdir -p "$plugins"
ln -sfn "$src" "$dest"
echo "linked $dest -> $src"

# The shell learns about a new plugin folder only on a rescan, which runs
# in the background (as `omarchy plugin add` does it)
omarchy-shell shell rescanPlugins >/dev/null
for ((attempt = 0; attempt < 40; attempt++)); do
  omarchy plugin list --json | jq -e --arg id "$id" 'any(.[]; .id == $id)' >/dev/null && break
  sleep 0.05
done
omarchy plugin enable "$id"
