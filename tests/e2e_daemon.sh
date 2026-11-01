#!/bin/bash
set -euo pipefail

# E2E: the daemon captures through the fake `wl-paste --watch`, serves the
# Unix socket (PING over socat and `nc -U`), stops cleanly, and its systemd
# unit verifies. rcopy/rpaste are in tests/e2e_remote.sh.
# Run from project root: ./tests/e2e_daemon.sh
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
STORE="$ISOLATED_ROOT/store"
SOCK="$XDG_RUNTIME_DIR/clipstack.sock"
LOG="$ISOLATED_ROOT/daemon.log"

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
# An nc that speaks Unix sockets (OpenBSD netcat)
have_nc_unix() {
    command -v nc >/dev/null || return 1
    local help
    help="$(nc -h 2>&1 || true)"
    [[ "$help" == *"-U"* ]]
}

# offer STATE CONTENT: the fake watcher reports a new selection
OFFERS=0
offer() {
    OFFERS=$((OFFERS + 1))
    local file="$FAKE_CLIP_DIR/offer-$OFFERS"
    printf '%s' "$2" >"$file"
    # A write to a FIFO with no reader would block forever
    timeout 5 sh -c 'printf "%s %s\n" "$1" "$2" > "$3"' sh "$1" "$file" "$FAKE_CLIP_DIR/events" ||
        log_fail "fake wl-paste --watch is not reading its events FIFO"
}

log_info "Starting an isolated daemon"
"$CLIPSTACK" --storage-dir "$STORE" daemon 2>"$LOG" &
DAEMON_PID=$!
track_pid "$DAEMON_PID"
wait_for 5 test -S "$SOCK" || log_fail "daemon socket never appeared at $SOCK"
wait_for 5 test -f "$FAKE_CLIP_DIR/watch.pid" || log_fail "daemon never ran wl-paste --watch"
WATCH_PID="$(cat "$FAKE_CLIP_DIR/watch.pid")"
log_pass "Daemon serves $SOCK and runs the watcher (pid $WATCH_PID)"

[[ "$(stat -c %a "$SOCK")" == 600 ]] && log_pass "Socket is 0600" || log_fail "Socket mode is $(stat -c %a "$SOCK")"

# --- capture -----------------------------------------------------------------
offer data "copied in e2e"
wait_for 5 has_entry "copied in e2e" watch && log_pass "Watch capture stores a copy" ||
    log_fail "watch capture did not store the copy"

offer sensitive "e2e-hunter2"
offer data "after the secret"
wait_for 5 has_entry "after the secret" || log_fail "capture stopped after a sensitive offer"
has_entry "e2e-hunter2" && log_fail "sensitive offer was stored" || log_pass "Sensitive offer is not stored"

# --- raw socket ----------------------------------------------------------------
if command -v socat >/dev/null; then
    reply="$(printf 'CLIPSTACK/1 PING 0\n' | socat -t 5 - "UNIX-CONNECT:$SOCK")"
    [[ "$reply" == "OK 0 version="*" capture=watch pull=0" ]] && log_pass "PING over socat: $reply" ||
        log_fail "PING over socat replied: $reply"
else
    log_skip "PING over socat (socat not installed)"
fi
if have_nc_unix; then
    reply="$(printf 'CLIPSTACK/1 PING 0\n' | timeout 5 nc -U "$SOCK")"
    [[ "$reply" == "OK 0 version="* ]] && log_pass "PING over nc -U" || log_fail "PING over nc -U replied: $reply"
else
    log_skip "PING over nc -U (no nc with -U)"
fi

# rcopy and rpaste over this socket: tests/e2e_remote.sh

# --- shutdown ------------------------------------------------------------------
kill -TERM "$DAEMON_PID"
wait "$DAEMON_PID" && log_pass "Daemon exits 0 on SIGTERM" || log_fail "daemon exit status $?"
[[ ! -e "$SOCK" ]] && log_pass "Socket removed on shutdown" || log_fail "socket left at $SOCK"
[[ ! -e "/proc/$WATCH_PID" ]] && log_pass "Watch child stopped with the daemon" ||
    log_fail "watch child $WATCH_PID still running"

# --- systemd unit ----------------------------------------------------------------
UNIT="$ROOT/systemd/clipstack.service"
[[ -f "$UNIT" ]] || log_fail "missing $UNIT"
if command -v systemd-analyze >/dev/null; then
    # verify checks that ExecStart exists: %h is the isolated HOME here
    mkdir -p "$HOME/.cargo/bin"
    ln -sf "$CLIPSTACK" "$HOME/.cargo/bin/clipstack"
    # verify also loads the user's other units; only our unit's lines count
    out="$(systemd-analyze --user verify "$UNIT" 2>&1 || true)"
    ours="$(printf '%s\n' "$out" | grep -F 'clipstack.service' || true)"
    [[ -z "$ours" ]] && log_pass "systemd-analyze --user verify accepts clipstack.service" ||
        log_fail "systemd-analyze --user verify: $ours"
else
    log_skip "systemd-analyze verify (systemd-analyze not installed)"
fi

echo
echo -e "${GREEN}All daemon E2E tests passed${NC}"
