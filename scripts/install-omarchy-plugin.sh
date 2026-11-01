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
plugins=${XDG_CONFIG_HOME:-$HOME/.config}/omarchy/plugins
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
omarchy plugin enable "$id"
