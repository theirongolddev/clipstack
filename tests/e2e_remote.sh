#!/bin/bash
set -euo pipefail

# E2E: scripts/rcopy and scripts/rpaste against an isolated daemon. On a
# remote host they reach the daemon through an ssh RemoteForward; here
# CLIPSTACK_SOCK points straight at the daemon's socket, which stands in for
# the forwarded one. Both transports available locally are used (socat and
# OpenBSD `nc -U`).
# Run from project root: ./tests/e2e_remote.sh
# Fully isolated (tests/lib.sh): temp runtime dir, store and HOME, fake
# wl-clipboard first on PATH, no WAYLAND_DISPLAY.

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
isolate_env

ROOT="$(pwd)"
CLIPSTACK="$ROOT/target/release/clipstack"
RCOPY="$ROOT/scripts/rcopy"
RPASTE="$ROOT/scripts/rpaste"
STORE="$ISOLATED_ROOT/store"
SOCK="$XDG_RUNTIME_DIR/clipstack.sock"
LOG="$ISOLATED_ROOT/daemon.log"
DAEMON_PID=""

log_pass() { echo -e "${GREEN}✓${NC} $1"; }
log_fail() {
    echo -e "${RED}✗${NC} $1"
    [[ -f "$LOG" ]] && sed 's/^/    daemon: /' "$LOG" >&2
    exit 1
}
log_info() { echo -e "${YELLOW}→${NC} $1"; }
log_skip() { echo -e "${YELLOW}-${NC} $1 (skipped)"; }

# cleanup (tests/lib.sh) stops only tracked PIDs and removes the temp root
trap cleanup EXIT

if [[ ! -f "$CLIPSTACK" ]]; then
    log_info "Building release binary..."
    cargo build --release
fi
command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }

# wait_for SECONDS CMD...: poll CMD every 50 ms until it succeeds
wait_for() {
    local deadline=$((SECONDS + $1))
    shift
    until "$@"; do
        ((SECONDS < deadline)) || return 1
        sleep 0.05
    done
}

export_json() { "$CLIPSTACK" --storage-dir "$STORE" export | jq -s .; }
# has_entry CONTENT [SOURCE]
has_entry() {
    export_json | jq -e --arg c "$1" --arg s "${2:-}" \
        'any(.[]; .content == $c and ($s == "" or .source == $s))' >/dev/null
}
clipboard() { cat "$FAKE_CLIP_DIR/content" 2>/dev/null; }
# set_clipboard FILE [TYPE...]: the fake clipboard offers FILE's bytes
set_clipboard() {
    cp "$1" "$FAKE_CLIP_DIR/content"
    shift
    printf '%s\n' text/plain\;charset=utf-8 "$@" >"$FAKE_CLIP_DIR/types"
}
# An nc that speaks Unix sockets (OpenBSD netcat)
have_nc_unix() {
    command -v nc >/dev/null || return 1
    local help
    help="$(nc -h 2>&1 || true)"
    [[ "$help" == *"-U"* ]]
}

# A PATH holding only the tools the scripts need plus the named transports
tool_path() {
    local dir="$ISOLATED_ROOT/path-$1" tool
    shift
    mkdir -p "$dir"
    for tool in sh cat mktemp wc tr rm sed grep "$@"; do
        if command -v "$tool" >/dev/null 2>&1 && [[ "$(type -t "$tool")" == file ]]; then
            ln -sf "$(command -v "$tool")" "$dir/$tool"
        fi
    done
    echo "$dir"
}

# start_daemon CONFIG_TOML: (re)start the isolated daemon with this config
start_daemon() {
    stop_daemon
    mkdir -p "$XDG_CONFIG_HOME/clipstack"
    printf '%s\n' "$1" >"$XDG_CONFIG_HOME/clipstack/config.toml"
    "$CLIPSTACK" --storage-dir "$STORE" daemon 2>>"$LOG" &
    DAEMON_PID=$!
    track_pid "$DAEMON_PID"
    wait_for 5 test -S "$SOCK" || log_fail "daemon socket never appeared at $SOCK"
}

stop_daemon() {
    [[ -n "$DAEMON_PID" ]] || return 0
    kill -TERM "$DAEMON_PID"
    wait "$DAEMON_PID" || log_fail "daemon exit status $?"
    DAEMON_PID=""
}

export CLIPSTACK_SOCK="$SOCK"
TRANSPORTS=()
command -v socat >/dev/null && TRANSPORTS+=(socat) || log_skip "socat transport (socat not installed)"
have_nc_unix && TRANSPORTS+=(nc) || log_skip "nc -U transport (no nc with -U)"
((${#TRANSPORTS[@]} > 0)) || log_fail "neither socat nor nc -U is installed; nothing to test"

log_info "Starting an isolated daemon (allow_pull = false)"
start_daemon '[storage]
ignore_patterns = ["^e2e-ignored"]'

# --- rcopy ---------------------------------------------------------------------
for t in "${TRANSPORTS[@]}"; do
    text=$'from rcopy\nvia '"$t"
    out="$(printf '%s' "$text" | PATH="$(tool_path "$t" "$t")" "$RCOPY" 2>&1)" ||
        log_fail "rcopy via $t failed: $out"
    has_entry "$text" remote && [[ "$(clipboard)" == "$text" ]] &&
        log_pass "rcopy via $t stores and copies" || log_fail "rcopy via $t: not stored or not copied"
done

set +e
err="$(printf 'e2e-ignored token' | "$RCOPY" 2>&1 >/dev/null)"
status=$?
set -e
[[ $status -eq 0 && "$err" == *"not saved"*"ignored"* ]] && log_pass "rcopy: stored=0 is reported on stderr, exit 0: $err" ||
    log_fail "ignored push: exit $status, stderr: $err"
[[ "$(clipboard)" == "e2e-ignored token" ]] && ! has_entry "e2e-ignored token" &&
    log_pass "rcopy: ignored push is copied but not stored" || log_fail "ignored push: wrong clipboard or stored"

set +e
err="$(printf '\377\376' | "$RCOPY" 2>&1 >/dev/null)"
status=$?
set -e
[[ $status -eq 1 && "$err" == *"not_utf8"* ]] && log_pass "rcopy: ERR reply exits 1: $err" ||
    log_fail "non-UTF-8 push: exit $status, stderr: $err"

# --- rpaste, pull disabled -----------------------------------------------------
printf 'local secret-free text' >"$ISOLATED_ROOT/clip"
set_clipboard "$ISOLATED_ROOT/clip"
set +e
out="$("$RPASTE" 2>"$ISOLATED_ROOT/err")"
status=$?
set -e
err="$(cat "$ISOLATED_ROOT/err")"
[[ $status -eq 1 && -z "$out" && "$err" == *"forbidden"*"allow_pull"* ]] &&
    log_pass "rpaste with allow_pull = false exits 1: $err" ||
    log_fail "rpaste without allow_pull: exit $status, stdout: $out, stderr: $err"

# --- rpaste, pull enabled ------------------------------------------------------
log_info "Restarting the daemon with allow_pull = true"
start_daemon '[socket]
allow_pull = true'

i=0
for t in "${TRANSPORTS[@]}"; do
    i=$((i + 1))
    # No trailing newline, then one with two; a tab, UTF-8 and a CR in between
    for case in "no-newline" "newlines"; do
        expected="$ISOLATED_ROOT/expected-$t-$case"
        printf 'pulled via %s\tcafé\r\n€ line two' "$t" >"$expected"
        [[ $case == newlines ]] && printf '\n\n' >>"$expected"
        set_clipboard "$expected"
        got="$ISOLATED_ROOT/got-$t-$case"
        PATH="$(tool_path "$t" "$t")" "$RPASTE" >"$got" 2>"$ISOLATED_ROOT/err" ||
            log_fail "rpaste via $t ($case) failed: $(cat "$ISOLATED_ROOT/err")"
        cmp -s "$expected" "$got" && log_pass "rpaste via $t prints the clipboard byte-for-byte ($case)" ||
            log_fail "rpaste via $t ($case): got $(od -c "$got" | head -3)"
    done
done

# An empty clipboard is empty output, exit 0
: >"$ISOLATED_ROOT/empty"
set_clipboard "$ISOLATED_ROOT/empty"
out="$("$RPASTE")" && [[ -z "$out" ]] && log_pass "rpaste of an empty clipboard prints nothing, exit 0" ||
    log_fail "rpaste of an empty clipboard: $out"

# A password manager's offer is refused even with pull on
printf 'hunter2' >"$ISOLATED_ROOT/secret"
set_clipboard "$ISOLATED_ROOT/secret" x-kde-passwordManagerHint
set +e
out="$("$RPASTE" 2>"$ISOLATED_ROOT/err")"
status=$?
set -e
err="$(cat "$ISOLATED_ROOT/err")"
[[ $status -eq 1 && -z "$out" && "$err" == *"sensitive"* ]] && log_pass "rpaste refuses a sensitive offer: $err" ||
    log_fail "sensitive pull: exit $status, stdout: $out, stderr: $err"

# rpaste doesn't add to the history
has_entry "hunter2" && log_fail "pulled secret was stored" || true
# Only the rcopy pushes (one per transport) are in it
[[ "$(export_json | jq length)" -eq ${#TRANSPORTS[@]} ]] && log_pass "Pulls are not saved to the history" ||
    log_fail "history changed: $(export_json)"

# --- unreachable -------------------------------------------------------------------
for script in "$RCOPY" "$RPASTE"; do
    name="$(basename "$script")"
    set +e
    err="$(printf 'x' | CLIPSTACK_SOCK="$ISOLATED_ROOT/missing.sock" "$script" 2>&1)"
    status=$?
    set -e
    [[ $status -eq 3 && "$err" == *"missing.sock"* ]] && log_pass "$name: missing socket exits 3" ||
        log_fail "$name, missing socket: exit $status, stderr: $err"

    set +e
    err="$(printf 'x' | PATH="$(tool_path none)" "$script" 2>&1)"
    status=$?
    set -e
    [[ $status -eq 3 && "$err" == *"socat"* ]] && log_pass "$name: no transport exits 3" ||
        log_fail "$name, no transport: exit $status, stderr: $err"
done

stop_daemon
# The daemon is gone but its socket file is left behind (like a stale forward)
if command -v python3 >/dev/null; then
    python3 -c 'import socket,sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$SOCK"
    DEAD_SOCKET_SCRIPTS=("$RCOPY" "$RPASTE")
else
    log_skip "dead socket (python3 not installed)"
    DEAD_SOCKET_SCRIPTS=()
fi
for script in "${DEAD_SOCKET_SCRIPTS[@]}"; do
    name="$(basename "$script")"
    set +e
    err="$(printf 'x' | "$script" 2>&1)"
    status=$?
    set -e
    [[ $status -eq 3 && "$err" == *"no reply"* ]] && log_pass "$name: dead socket exits 3" ||
        log_fail "$name, dead socket: exit $status, stderr: $err"
done

echo
echo -e "${GREEN}All remote E2E tests passed${NC}"
