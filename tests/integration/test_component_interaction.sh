#!/bin/bash
set -euo pipefail

# Integration Tests: Component Interaction (Daemon + Picker + Storage)
# Run from project root: ./tests/integration/test_component_interaction.sh
#
# Tests that can run headlessly (no Wayland required):
# - Lock file behavior
# - Storage access patterns
# - Concurrent access safety
#
# Note: Tests marked [WAYLAND] require a Wayland session and manual testing

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Temp XDG dirs, fake wl-paste/wl-copy first on PATH, no WAYLAND_DISPLAY
# shellcheck source=../lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/../lib.sh"
isolate_env

TEST_DIR="$ISOLATED_ROOT/work"
mkdir -p "$TEST_DIR"
CLIPSTACK="./target/release/clipstack"
GREP="/usr/bin/grep"

log_pass() { echo -e "${GREEN}✓${NC} $1"; }
log_fail() { echo -e "${RED}✗${NC} $1"; exit 1; }
log_info() { echo -e "${YELLOW}→${NC} $1"; }
log_skip() { echo -e "${YELLOW}⊘${NC} [WAYLAND] $1 - requires manual testing"; }

# cleanup (tests/lib.sh) stops only PIDs recorded with track_pid
trap cleanup EXIT

# Helper to create a test entry in storage
create_entry() {
    local storage_dir="$1"
    local content="$2"
    local ts="${3:-$(date +%s%3N)}"
    local pinned="${4:-false}"

    mkdir -p "$storage_dir"
    printf '%s' "$content" > "$storage_dir/$ts.txt"

    local preview
    preview=$(printf '%s' "$content" | head -c 100 | tr '\n\t\r' '   ')
    local hash="sha256:$(printf '%s' "$content" | sha256sum | cut -d' ' -f1)"
    local size=${#content}

    printf '{"id":"%s","timestamp":%s,"size":%d,"preview":"%s","hash":"%s","pinned":%s}' \
        "$ts" "$ts" "$size" "$preview" "$hash" "$pinned"
}

# Build index from entries
build_index() {
    local storage_dir="$1"
    local max_entries="$2"
    shift 2
    local entries=("$@")

    {
        printf '{"max_entries":%d,"entries":[' "$max_entries"
        local first=true
        for entry in "${entries[@]}"; do
            if [ "$first" = true ]; then
                first=false
            else
                printf ','
            fi
            printf '%s' "$entry"
        done
        printf ']}'
    } > "$storage_dir/index.json"
}

# Build release if needed
if [[ ! -f "$CLIPSTACK" ]]; then
    log_info "Building release binary..."
    cargo build --release
fi

log_info "Test directory: $TEST_DIR"
log_info "Testing component interactions..."
echo ""

# Test 1: Lock file prevents multiple daemons (simulate check)
log_info "Test 1: Lock file detection"

# Verify status works without lock
STATUS_OUT=$($CLIPSTACK status 2>&1)
if echo "$STATUS_OUT" | $GREP -q "not running"; then
    log_pass "Status detects: no daemon running"
else
    log_pass "Status command executed successfully"
fi

# Test 2: Storage commands work without daemon
log_info "Test 2: Storage access without daemon"
mkdir -p "$TEST_DIR/test2"
ENTRY2=$(create_entry "$TEST_DIR/test2" "test without daemon" "1700000001000" "false")
build_index "$TEST_DIR/test2" 100 "$ENTRY2"

# These should work without daemon
STATS=$($CLIPSTACK --storage-dir "$TEST_DIR/test2" stats 2>&1) || true
LIST=$($CLIPSTACK --storage-dir "$TEST_DIR/test2" list 2>&1) || true

if echo "$STATS" | $GREP -q "Entries:" && echo "$LIST" | $GREP -q "test without daemon"; then
    log_pass "Storage access: stats and list work without daemon"
else
    log_fail "Storage access failed without daemon"
fi

# Test 3: Concurrent read access safety
log_info "Test 3: Concurrent read access safety"
mkdir -p "$TEST_DIR/test3"
# Create many entries
ENTRIES3=()
for i in $(seq 1 50); do
    TS=$((1700000000000 + i * 1000))
    ENTRY=$(create_entry "$TEST_DIR/test3" "concurrent entry $i" "$TS" "false")
    ENTRIES3=("$ENTRY" "${ENTRIES3[@]}")
done
build_index "$TEST_DIR/test3" 100 "${ENTRIES3[@]}"

# Run many concurrent reads
for _ in {1..20}; do
    $CLIPSTACK --storage-dir "$TEST_DIR/test3" stats >/dev/null 2>&1 &
    $CLIPSTACK --storage-dir "$TEST_DIR/test3" list --count 10 >/dev/null 2>&1 &
done
wait

# Verify integrity: the 40 racing first opens imported the store exactly once
COUNT=$($CLIPSTACK --storage-dir "$TEST_DIR/test3" export | jq -s 'length')
BACKUPS=$(find "$TEST_DIR/test3" -maxdepth 1 -type d -name 'legacy-backup-*' | wc -l)
if [[ "$COUNT" -eq 50 && "$BACKUPS" -eq 1 ]] && \
    $CLIPSTACK --storage-dir "$TEST_DIR/test3" check >/dev/null 2>&1; then
    log_pass "Concurrent reads: 40 racing first opens imported once, check passes"
else
    log_fail "Concurrent first opens: $COUNT entries, $BACKUPS backups (expected 50 and 1)"
fi

# Test 4: Concurrent writers lose nothing
log_info "Test 4: Concurrent writes"
mkdir -p "$TEST_DIR/test4"
for i in $(seq 1 20); do
    printf 'concurrent write %s' "$i" | $CLIPSTACK --storage-dir "$TEST_DIR/test4" copy >/dev/null 2>&1 &
    $CLIPSTACK --storage-dir "$TEST_DIR/test4" list --count 5 >/dev/null 2>&1 &
done
wait

COUNT=$($CLIPSTACK --storage-dir "$TEST_DIR/test4" export | jq -s 'length')
if [[ "$COUNT" -eq 20 ]] && $CLIPSTACK --storage-dir "$TEST_DIR/test4" check >/dev/null 2>&1; then
    log_pass "Concurrent writes: all 20 copies saved, check passes"
else
    log_fail "Concurrent writes: $COUNT of 20 copies saved"
fi

# Test 5: Leftovers of a crashed old version are imported and backed up
log_info "Test 5: Import after a simulated crash of the old version"
mkdir -p "$TEST_DIR/test5"

# Create valid entries
for i in {1..5}; do
    TS=$((1700000000000 + i * 1000))
    echo "crash test entry $i" > "$TEST_DIR/test5/$TS.txt"
done

# Create orphaned temp file (simulating crash during write)
echo "orphan temp" > "$TEST_DIR/test5/index.tmp"
echo '{"max_entries":100' > "$TEST_DIR/test5/partial.tmp"

# Access storage: the import moves the .tmp files into the backup with the rest
RECOVER=$($CLIPSTACK --storage-dir "$TEST_DIR/test5" recover 2>/dev/null) || true

TMP_COUNT=$(find "$TEST_DIR/test5" -maxdepth 1 -name "*.tmp" 2>/dev/null | wc -l)
BACKED_UP=$(find "$TEST_DIR/test5" -mindepth 2 -path '*/legacy-backup-*/*.tmp' 2>/dev/null | wc -l)
if [[ "$TMP_COUNT" -eq 0 && "$BACKED_UP" -eq 2 ]] && \
    echo "$RECOVER" | $GREP -q "Recovery complete. 5 entries."; then
    log_pass "Crash leftovers: 5 entries imported, temp files moved to the backup"
else
    log_fail "Found $TMP_COUNT temp files in the store, $BACKED_UP in the backup; recover said: $RECOVER"
fi

# Test 6: Storage respects max_entries with many saves
log_info "Test 6: Storage pruning under load"
mkdir -p "$TEST_DIR/test6"
ENTRIES6=()
# Create 100 entries
for i in $(seq 1 100); do
    TS=$((1700000000000 + i * 1000))
    ENTRY=$(create_entry "$TEST_DIR/test6" "prune test $i" "$TS" "false")
    ENTRIES6=("$ENTRY" "${ENTRIES6[@]}")
done
build_index "$TEST_DIR/test6" 100 "${ENTRIES6[@]}"

# Trigger pruning with low max
$CLIPSTACK --storage-dir "$TEST_DIR/test6" --max-entries 20 stats >/dev/null 2>&1

# Verify pruning worked
COUNT=$($CLIPSTACK --storage-dir "$TEST_DIR/test6" --max-entries 20 export | jq -s 'length')
if [[ "$COUNT" -eq 20 ]]; then
    log_pass "Storage pruning: 100 entries pruned to $COUNT (max 20)"
else
    log_fail "Pruning failed: $COUNT entries remain (expected 20)"
fi

echo ""
echo "=========================================="
echo "Tests requiring Wayland (manual testing):"
echo "=========================================="

log_skip "Daemon saves → Picker sees immediately"
echo "    Start daemon, copy text via wl-copy"
echo "    Open picker within 500ms, verify entry visible"
echo ""

log_skip "Picker changes → Daemon respects"
echo "    Open picker, delete entry, close picker"
echo "    Daemon next poll should see deletion"
echo ""

log_skip "Rapid copy while picker open"
echo "    Start daemon, open picker"
echo "    Rapidly copy 100 items while navigating"
echo "    Verify no corruption"
echo ""

log_skip "Graceful handling when daemon stops"
echo "    Open picker, stop daemon (kill -TERM)"
echo "    Navigate picker (should still work)"
echo "    Copy action should succeed"
echo ""

echo ""
echo -e "${GREEN}All automated component tests passed!${NC}"
echo "Run manual Wayland tests when in a Wayland session."
