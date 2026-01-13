#!/bin/bash
set -euo pipefail

# E2E Tests for Atomic File Writes and Recovery
# Run from project root: ./tests/e2e_atomic_writes.sh
#
# Note: Creates entries directly in storage (doesn't require wl-clipboard)

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

cleanup() { rm -rf "$TEST_DIR"; }
trap cleanup EXIT

# Helper to create test entries directly in storage
create_test_entries() {
    local storage_dir="$1"
    local count="$2"
    local max_entries="${3:-100}"

    mkdir -p "$storage_dir"
    local entries=""
    for i in $(seq "$count" -1 1); do
        local ts=$((1700000000000 + i * 1000))
        local content="test entry $i"
        echo "$content" > "$storage_dir/$ts.txt"
        local hash="sha256:$(echo -n "$content" | sha256sum | cut -d' ' -f1)"
        local size=${#content}
        if [ -n "$entries" ]; then entries="$entries,"; fi
        entries="$entries{\"id\":\"$ts\",\"timestamp\":$ts,\"size\":$size,\"preview\":\"$content\",\"hash\":\"$hash\"}"
    done
    echo "{\"max_entries\":$max_entries,\"entries\":[$entries]}" > "$storage_dir/index.json"
}

# Build release if needed
if [[ ! -f "$CLIPSTACK" ]]; then
    log_info "Building release binary..."
    cargo build --release
fi

log_info "Test directory: $TEST_DIR"

# Test 1: Temp file cleanup on startup
log_info "Test 1: Temp file cleanup on startup"
mkdir -p "$TEST_DIR/test1"
touch "$TEST_DIR/test1/index.tmp"
touch "$TEST_DIR/test1/orphan.tmp"
touch "$TEST_DIR/test1/another.tmp"
# Running any command should clean up .tmp files
$CLIPSTACK --storage-dir "$TEST_DIR/test1" stats >/dev/null 2>&1
TEMP_COUNT=$(find "$TEST_DIR/test1" -name "*.tmp" 2>/dev/null | wc -l)
[[ "$TEMP_COUNT" -eq 0 ]] && \
    log_pass "Orphaned .tmp files cleaned up" || \
    log_fail "Found $TEMP_COUNT .tmp files remaining"

# Test 2: No temp files after normal operations
log_info "Test 2: No temp files after storage operations"
create_test_entries "$TEST_DIR/test2" 10 100
# Simulate storage access
$CLIPSTACK --storage-dir "$TEST_DIR/test2" stats >/dev/null 2>&1
$CLIPSTACK --storage-dir "$TEST_DIR/test2" list >/dev/null 2>&1
TEMP_COUNT=$(find "$TEST_DIR/test2" -name "*.tmp" 2>/dev/null | wc -l)
[[ "$TEMP_COUNT" -eq 0 ]] && \
    log_pass "No .tmp files remain after operations" || \
    log_fail "Found $TEMP_COUNT .tmp files"

# Test 3: Recovery from missing index
log_info "Test 3: Recovery from missing index"
mkdir -p "$TEST_DIR/test3"
echo "orphan content 1" > "$TEST_DIR/test3/1700000001000.txt"
echo "orphan content 2" > "$TEST_DIR/test3/1700000002000.txt"
echo "orphan content 3" > "$TEST_DIR/test3/1700000003000.txt"
# No index.json - just orphaned content files
$CLIPSTACK --storage-dir "$TEST_DIR/test3" recover 2>&1 | $GREP -q "Recovered" && \
    log_pass "Recovery detected orphaned files" || true
# Verify recovery created a valid index
COUNT=$($CLIPSTACK --storage-dir "$TEST_DIR/test3" stats 2>/dev/null | $GREP "^Entries:" | awk '{print $2}' | cut -d'/' -f1)
[[ "$COUNT" -ge 1 ]] && \
    log_pass "Recovered $COUNT entries from orphaned files" || \
    log_fail "Recovery failed to rebuild index"

# Test 4: Recovery from corrupted index
log_info "Test 4: Recovery from corrupted index"
mkdir -p "$TEST_DIR/test4"
echo "valid content file" > "$TEST_DIR/test4/1700000004000.txt"
echo "{ this is not valid json ]]{{" > "$TEST_DIR/test4/index.json"
$CLIPSTACK --storage-dir "$TEST_DIR/test4" recover 2>&1 | $GREP -q "Recovered\|complete" && \
    log_pass "Recovery handled corrupted index" || true
# Verify we can now read the storage
$CLIPSTACK --storage-dir "$TEST_DIR/test4" stats >/dev/null 2>&1 && \
    log_pass "Storage usable after recovery from corruption" || \
    log_fail "Storage unusable after recovery attempt"

# Test 5: Recovery deduplicates by hash
log_info "Test 5: Recovery deduplicates identical content"
mkdir -p "$TEST_DIR/test5"
echo "duplicate content" > "$TEST_DIR/test5/1700000005000.txt"
echo "duplicate content" > "$TEST_DIR/test5/1700000006000.txt"
echo "duplicate content" > "$TEST_DIR/test5/1700000007000.txt"
$CLIPSTACK --storage-dir "$TEST_DIR/test5" recover >/dev/null 2>&1
COUNT=$($CLIPSTACK --storage-dir "$TEST_DIR/test5" stats 2>/dev/null | $GREP "^Entries:" | awk '{print $2}' | cut -d'/' -f1)
# Should keep only 1 entry since all content is identical
[[ "$COUNT" -eq 1 ]] && \
    log_pass "Deduplicated to $COUNT entry" || \
    log_fail "Expected 1 entry after dedup, got $COUNT"

# Test 6: Index integrity after max_entries pruning
log_info "Test 6: Index integrity after pruning"
create_test_entries "$TEST_DIR/test6" 50 100
# Access with lower max_entries to trigger pruning
$CLIPSTACK --storage-dir "$TEST_DIR/test6" --max-entries 10 stats >/dev/null 2>&1
# Verify index is still valid JSON
if $CLIPSTACK --storage-dir "$TEST_DIR/test6" --max-entries 10 stats >/dev/null 2>&1; then
    log_pass "Index intact after pruning"
else
    log_fail "Index corrupted after pruning"
fi

# Test 7: Verify atomic write behavior with multiple operations
log_info "Test 7: Multiple sequential operations"
for i in $(seq 1 5); do
    create_test_entries "$TEST_DIR/test7" "$((i * 5))" 100
    $CLIPSTACK --storage-dir "$TEST_DIR/test7" stats >/dev/null 2>&1
done
# Final check - no temp files and valid index
TEMP_COUNT=$(find "$TEST_DIR/test7" -name "*.tmp" 2>/dev/null | wc -l)
VALID=$($CLIPSTACK --storage-dir "$TEST_DIR/test7" stats >/dev/null 2>&1 && echo "yes" || echo "no")
[[ "$TEMP_COUNT" -eq 0 && "$VALID" == "yes" ]] && \
    log_pass "No corruption after multiple operations" || \
    log_fail "Issues after multiple operations (tmp=$TEMP_COUNT, valid=$VALID)"

echo ""
echo -e "${GREEN}All E2E atomic writes tests passed!${NC}"
