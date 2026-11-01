# shellcheck shell=bash
# Shared isolation for shell E2E suites (the shell twin of tests/common/mod.rs).
#
#   source "$(dirname "${BASH_SOURCE[0]}")/../lib.sh"   # path relative to the script
#   isolate_env
#   trap cleanup EXIT
#   "$CLIPSTACK" daemon & track_pid $!
#
# isolate_env points XDG_RUNTIME_DIR, XDG_DATA_HOME, XDG_CONFIG_HOME and HOME
# at a fresh temp dir, puts tests/fakes first on PATH (fake wl-paste/wl-copy)
# and unsets WAYLAND_DISPLAY, so a suite can't reach the user's clipboard,
# store (~/.local/share/clipd) or daemon lock. cleanup kills only the PIDs
# recorded with track_pid. Never use pkill/killall in a suite.

CLIPSTACK_TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ISOLATED_ROOT=""
TRACKED_PIDS=()

isolate_env() {
    ISOLATED_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/clipstack-e2e.XXXXXX")"

    # Keep the toolchain usable after HOME moves (scripts may run cargo)
    export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
    export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"

    export XDG_RUNTIME_DIR="$ISOLATED_ROOT/runtime"
    export XDG_DATA_HOME="$ISOLATED_ROOT/data"
    export XDG_CONFIG_HOME="$ISOLATED_ROOT/config"
    export HOME="$ISOLATED_ROOT/home"
    export FAKE_CLIP_DIR="$ISOLATED_ROOT/fake-clip"
    mkdir -p "$XDG_RUNTIME_DIR" "$XDG_DATA_HOME" "$XDG_CONFIG_HOME" "$HOME" "$FAKE_CLIP_DIR"
    chmod 700 "$XDG_RUNTIME_DIR"

    export PATH="$CLIPSTACK_TESTS_DIR/fakes:$PATH"
    unset WAYLAND_DISPLAY WAYLAND_SOCKET CLIPSTACK_MAX_ENTRIES

    case "$XDG_RUNTIME_DIR" in
        /run/user*)
            echo "isolate_env: refusing to use real runtime dir $XDG_RUNTIME_DIR" >&2
            exit 1
            ;;
    esac
    if [[ "$(command -v wl-paste)" != "$CLIPSTACK_TESTS_DIR/fakes/wl-paste" ]]; then
        echo "isolate_env: fake wl-paste is not first on PATH" >&2
        exit 1
    fi
}

# Record a PID so cleanup can stop it (and only it)
track_pid() {
    TRACKED_PIDS+=("$1")
}

cleanup() {
    local pid
    for pid in "${TRACKED_PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
    TRACKED_PIDS=()
    if [[ -n "$ISOLATED_ROOT" && "$ISOLATED_ROOT" == "${TMPDIR:-/tmp}"/clipstack-e2e.* ]]; then
        rm -rf -- "$ISOLATED_ROOT"
    fi
}
