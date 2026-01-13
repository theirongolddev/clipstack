#!/bin/bash
set -euo pipefail

# E2E Tests for Configurable Max Entries
# Run from project root: ./tests/e2e_max_entries.sh
# Note: Tests create storage directly to work without Wayland/wl-clipboard

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

TEST_DIR=$(mktemp -d)
CLIPSTACK="./target/release/clipstack"

log_pass() { echo -e "${GREEN}✓${NC} $1"; }
log_fail() { echo -e "${RED}✗${NC} $1"; exit 1; }
log_info() { echo -e "${YELLOW}→${NC} $1"; }

cleanup() { rm -rf "$TEST_DIR"; }
trap cleanup EXIT

# Build release if needed
if [[ ! -f "$CLIPSTACK" ]]; then
    log_info "Building release binary..."
    cargo build --release
fi

log_info "Test directory: $TEST_DIR"

# Test 1: Default max_entries is 100 (check via status command's Config section)
log_info "Test 1: Default max_entries"
$CLIPSTACK --storage-dir "$TEST_DIR/test1" status 2>/dev/null | /bin/grep -q "Max entries: 100 (default)" && \
    log_pass "Default is 100" || log_fail "Default should be 100"

# Test 2: CLI flag overrides default
log_info "Test 2: CLI flag"
$CLIPSTACK --storage-dir "$TEST_DIR/test2" --max-entries 50 status 2>/dev/null | /bin/grep -q "Max entries: 50" && \
    log_pass "CLI flag works" || log_fail "CLI flag should set max_entries"

# Test 3: Environment variable works
log_info "Test 3: Environment variable"
CLIPSTACK_MAX_ENTRIES=75 $CLIPSTACK --storage-dir "$TEST_DIR/test3" status 2>/dev/null | /bin/grep -q "Max entries: 75 (env)" && \
    log_pass "Env var works" || log_fail "Env var should set max_entries"

# Test 4: CLI takes precedence over env var
log_info "Test 4: CLI precedence"
CLIPSTACK_MAX_ENTRIES=100 $CLIPSTACK --storage-dir "$TEST_DIR/test4" --max-entries 200 status 2>/dev/null | \
    /bin/grep -q "Max entries: 200" && \
    log_pass "CLI overrides env" || log_fail "CLI should override env var"

# Test 5: Entries are pruned when limit exceeded
# Note: Creates storage directly since copy command needs wl-clipboard
log_info "Test 5: Pruning works"
mkdir -p "$TEST_DIR/test5"
# Create 20 content files and index
for i in $(seq 1 20); do
    TS=$((1700000000000 + i * 1000))
    echo "entry $i content" > "$TEST_DIR/test5/$TS.txt"
done
# Build index JSON
{
    echo '{"max_entries":20,"entries":['
    for i in $(seq 20 -1 1); do
        TS=$((1700000000000 + i * 1000))
        [[ $i -gt 1 ]] && COMMA="," || COMMA=""
        echo "{\"id\":\"$TS\",\"timestamp\":$TS,\"size\":16,\"preview\":\"entry $i content\",\"hash\":\"hash$i\"}$COMMA"
    done
    echo ']}'
} > "$TEST_DIR/test5/index.json"
# Access with max=10 triggers pruning
$CLIPSTACK --storage-dir "$TEST_DIR/test5" --max-entries 10 stats >/dev/null 2>&1
COUNT=$($CLIPSTACK --storage-dir "$TEST_DIR/test5" --max-entries 10 stats 2>/dev/null | /bin/grep "^Entries:" | awk '{print $2}' | cut -d'/' -f1)
[[ "$COUNT" -le 10 ]] && log_pass "Pruning works (20 -> $COUNT entries)" || \
    log_fail "Should have <=10 entries, got $COUNT"

# Test 6: Value clamping - low values rejected
log_info "Test 6: Value clamping (low)"
# clap rejects 0 with error message containing "is not in"
OUTPUT=$($CLIPSTACK --storage-dir "$TEST_DIR/test6" --max-entries 0 stats 2>&1 || true)
echo "$OUTPUT" | /bin/grep -q "is not in" && \
    log_pass "Zero rejected by CLI" || log_fail "Should reject 0"

# Test 7: Value clamping - high values rejected
log_info "Test 7: Value clamping (high)"
OUTPUT=$($CLIPSTACK --storage-dir "$TEST_DIR/test7" --max-entries 50000 stats 2>&1 || true)
echo "$OUTPUT" | /bin/grep -q "is not in" && \
    log_pass "Values > 10000 rejected by CLI" || log_fail "Should reject values > 10000"

# Test 8: Reducing max_entries prunes existing data
# Note: Creates storage directly since copy command needs wl-clipboard
log_info "Test 8: Dynamic pruning on limit reduction"
mkdir -p "$TEST_DIR/test8"
# Create 15 content files and index
for i in $(seq 1 15); do
    TS=$((1700000000000 + i * 1000))
    echo "entry $i content" > "$TEST_DIR/test8/$TS.txt"
done
# Build index JSON with max_entries=100
{
    echo '{"max_entries":100,"entries":['
    for i in $(seq 15 -1 1); do
        TS=$((1700000000000 + i * 1000))
        [[ $i -gt 1 ]] && COMMA="," || COMMA=""
        echo "{\"id\":\"$TS\",\"timestamp\":$TS,\"size\":16,\"preview\":\"entry $i content\",\"hash\":\"hash$i\"}$COMMA"
    done
    echo ']}'
} > "$TEST_DIR/test8/index.json"
# Verify we have 15
BEFORE=$($CLIPSTACK --storage-dir "$TEST_DIR/test8" --max-entries 100 stats 2>/dev/null | /bin/grep "^Entries:" | awk '{print $2}' | cut -d'/' -f1)
[[ "$BEFORE" -eq 15 ]] || log_fail "Should have 15 entries before reduction, got $BEFORE"
# Now access with max=5 which should prune
$CLIPSTACK --storage-dir "$TEST_DIR/test8" --max-entries 5 stats 2>/dev/null
AFTER=$($CLIPSTACK --storage-dir "$TEST_DIR/test8" --max-entries 5 stats 2>/dev/null | /bin/grep "^Entries:" | awk '{print $2}' | cut -d'/' -f1)
[[ "$AFTER" -le 5 ]] && log_pass "Dynamic pruning works ($BEFORE -> $AFTER)" || \
    log_fail "Should prune to 5, got $AFTER"

echo ""
echo -e "${GREEN}All E2E tests passed!${NC}"
