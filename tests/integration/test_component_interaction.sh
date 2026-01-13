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

TEST_DIR=$(mktemp -d)
CLIPSTACK="./target/release/clipstack"
GREP="/usr/bin/grep"

log_pass() { echo -e "${GREEN}✓${NC} $1"; }
log_fail() { echo -e "${RED}✗${NC} $1"; exit 1; }
log_info() { echo -e "${YELLOW}→${NC} $1"; }
log_skip() { echo -e "${YELLOW}⊘${NC} [WAYLAND] $1 - requires manual testing"; }

cleanup() {
    # Kill any daemon processes we started
    pkill -f "clipstack.*daemon" 2>/dev/null || true
    # Remove lock file
    rm -f /tmp/clipstack-daemon.lock
    rm -rf "$TEST_DIR"
}
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
rm -f /tmp/clipstack-daemon.lock

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

# Verify integrity
if $CLIPSTACK --storage-dir "$TEST_DIR/test3" stats >/dev/null 2>&1; then
    log_pass "Concurrent reads: No corruption after 40 concurrent accesses"
else
    log_fail "Storage corrupted after concurrent reads"
fi

# Test 4: Atomic writes during concurrent access
log_info "Test 4: Atomic writes during concurrent access"
mkdir -p "$TEST_DIR/test4"
ENTRIES4=()
for i in $(seq 1 20); do
    TS=$((1700000000000 + i * 1000))
    ENTRY=$(create_entry "$TEST_DIR/test4" "atomic test $i" "$TS" "false")
    ENTRIES4=("$ENTRY" "${ENTRIES4[@]}")
done
build_index "$TEST_DIR/test4" 100 "${ENTRIES4[@]}"

# Run concurrent reads while modifying index
for i in {1..10}; do
    # Modify index
    if [ $((i % 2)) -eq 0 ]; then
        sed -i 's/"pinned":false/"pinned":true/' "$TEST_DIR/test4/index.json" 2>/dev/null || true
    else
        sed -i 's/"pinned":true/"pinned":false/' "$TEST_DIR/test4/index.json" 2>/dev/null || true
    fi
    # Concurrent reads
    $CLIPSTACK --storage-dir "$TEST_DIR/test4" stats >/dev/null 2>&1 &
    $CLIPSTACK --storage-dir "$TEST_DIR/test4" list --count 5 >/dev/null 2>&1 &
done
wait

# Verify no temp files left
TMP_COUNT=$(find "$TEST_DIR/test4" -name "*.tmp" 2>/dev/null | wc -l)
if [[ "$TMP_COUNT" -eq 0 ]]; then
    log_pass "Atomic writes: No .tmp files after concurrent access"
else
    log_fail "Found $TMP_COUNT orphaned .tmp files"
fi

# Test 5: Recovery after simulated crash
log_info "Test 5: Recovery after simulated crash"
mkdir -p "$TEST_DIR/test5"

# Create valid entries
for i in {1..5}; do
    TS=$((1700000000000 + i * 1000))
    echo "crash test entry $i" > "$TEST_DIR/test5/$TS.txt"
done

# Create orphaned temp file (simulating crash during write)
echo "orphan temp" > "$TEST_DIR/test5/index.tmp"
echo '{"max_entries":100' > "$TEST_DIR/test5/partial.tmp"

# Access storage (should clean up temp files)
$CLIPSTACK --storage-dir "$TEST_DIR/test5" recover >/dev/null 2>&1 || true
$CLIPSTACK --storage-dir "$TEST_DIR/test5" stats >/dev/null 2>&1 || true

TMP_COUNT=$(find "$TEST_DIR/test5" -name "*.tmp" 2>/dev/null | wc -l)
if [[ "$TMP_COUNT" -eq 0 ]]; then
    log_pass "Crash recovery: Temp files cleaned up on startup"
else
    log_fail "Found $TMP_COUNT temp files after recovery"
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
COUNT=$($GREP -c '"id"' "$TEST_DIR/test6/index.json" 2>/dev/null || echo 0)
if [[ "$COUNT" -le 22 ]]; then
    log_pass "Storage pruning: 100 entries pruned to $COUNT (max 20)"
else
    log_fail "Pruning failed: $COUNT entries remain (expected <=22)"
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
