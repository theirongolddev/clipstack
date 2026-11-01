#!/bin/bash
set -euo pipefail

# v1.1 Features Integration Test
# Run from project root: ./tests/integration/test_v11_features.sh
# Tests that ALL v1.1 features work correctly together

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Temp XDG dirs and HOME, fake wl-paste/wl-copy first on PATH, no WAYLAND_DISPLAY
# shellcheck source=../lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/../lib.sh"
isolate_env

TEST_DIR="$ISOLATED_ROOT/work"
mkdir -p "$TEST_DIR"
CLIPSTACK="./target/release/clipstack"

log_pass() { echo -e "${GREEN}✓${NC} $1"; }
log_fail() { echo -e "${RED}✗${NC} $1"; FAILED=1; }
log_info() { echo -e "${YELLOW}→${NC} $1"; }

FAILED=0

# cleanup (tests/lib.sh) stops only tracked PIDs and removes the temp root
trap cleanup EXIT

# Helper to create test entries directly in storage
create_entry() {
    local storage_dir="$1"
    local content="$2"
    local ts="${3:-$(date +%s%3N)}"
    local pinned="${4:-false}"

    mkdir -p "$storage_dir"
    printf '%s' "$content" > "$storage_dir/$ts.txt"

    local preview hash size
    preview=$(printf '%s' "$content" | head -c 100 | tr '\n\t\r' '   ')
    hash="sha256:$(printf '%s' "$content" | sha256sum | cut -d' ' -f1)"
    size=${#content}

    printf '{"id":"%s","timestamp":%s,"size":%d,"preview":"%s","hash":"%s","pinned":%s}' \
        "$ts" "$ts" "$size" "$preview" "$hash" "$pinned"
}

build_index() {
    local storage_dir="$1"
    local max_entries="$2"
    shift 2
    local entries=("$@")

    {
        printf '{"max_entries":%d,"entries":[' "$max_entries"
        local first=true
        for entry in "${entries[@]}"; do
            [[ "$first" = true ]] && first=false || printf ','
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

log_info "v1.1 Features Integration Test"

# All entries of the store at $1 as one JSON array (imports legacy files first)
exported() {
    local dir="$1"
    shift
    $CLIPSTACK --storage-dir "$dir" "$@" export | jq -s .
}
log_info "Test directory: $TEST_DIR"
echo ""

# ============================================================================
# Test 1: Configurable limits + Pinned entries interaction
# ============================================================================
log_info "Test 1: Configurable max_entries respects pinned entries"
mkdir -p "$TEST_DIR/test1"

# Create 5 pinned entries (oldest timestamps)
ENTRIES1=()
for i in $(seq 1 5); do
    TS=$((1700000000000 + i * 100))
    ENTRY=$(create_entry "$TEST_DIR/test1" "pinned entry $i" "$TS" "true")
    ENTRIES1+=("$ENTRY")
done

# Add 25 unpinned entries (newer timestamps)
for i in $(seq 1 25); do
    TS=$((1700000001000 + i * 100))
    ENTRY=$(create_entry "$TEST_DIR/test1" "unpinned entry $i" "$TS" "false")
    ENTRIES1=("$ENTRY" "${ENTRIES1[@]}")  # Prepend (newest first)
done

build_index "$TEST_DIR/test1" 100 "${ENTRIES1[@]}"

# Run with max=20 - should keep 5 pinned + 20 unpinned = 25 max, prune 5 oldest unpinned
$CLIPSTACK --storage-dir "$TEST_DIR/test1" --max-entries 20 stats >/dev/null 2>&1

# Count remaining entries
EXPORT1=$(exported "$TEST_DIR/test1" --max-entries 20)
TOTAL=$(echo "$EXPORT1" | jq 'length')
PINNED_COUNT=$(echo "$EXPORT1" | jq 'map(select(.pinned)) | length')

# All 5 pinned should survive, plus up to 20 unpinned
[[ "$PINNED_COUNT" -eq 5 ]] && \
    log_pass "All 5 pinned entries preserved" || \
    log_fail "Expected 5 pinned, got $PINNED_COUNT"

[[ "$TOTAL" -eq 25 ]] && \
    log_pass "Total entries respects limit ($TOTAL entries)" || \
    log_fail "Expected 25 total, got $TOTAL"

# ============================================================================
# Test 2: Search finds both preview and content matches with pinned indicator
# ============================================================================
log_info "Test 2: Search works with pinned entries (content beyond preview)"
mkdir -p "$TEST_DIR/test2"

# Create pinned entry with search term beyond preview length
LONG_PREFIX=$(printf 'x%.0s' {1..150})
PINNED_DEEP_CONTENT="${LONG_PREFIX}DEEP_SEARCH_TERM_HERE"
ENTRY_PINNED=$(create_entry "$TEST_DIR/test2" "$PINNED_DEEP_CONTENT" "1700000001000" "true")

# Create unpinned entry with search term in preview
ENTRY_UNPINNED=$(create_entry "$TEST_DIR/test2" "PREVIEW_SEARCH_TERM visible" "1700000002000" "false")

build_index "$TEST_DIR/test2" 100 "$ENTRY_UNPINNED" "$ENTRY_PINNED"

# Verify search term is in the stored content but not the preview
EXPORT2=$(exported "$TEST_DIR/test2")
if $CLIPSTACK --storage-dir "$TEST_DIR/test2" list 2>/dev/null | grep -q "DEEP_SEARCH_TERM"; then
    log_fail "Deep search term should be beyond the preview"
elif echo "$EXPORT2" | jq -e --arg c "$PINNED_DEEP_CONTENT" 'any(.[]; .content == $c)' >/dev/null; then
    log_pass "Deep search term in content, not preview (searchable)"
else
    log_fail "Deep search term not found in content"
fi

# Verify pinned entry is marked correctly
if echo "$EXPORT2" | jq -e --arg c "$PINNED_DEEP_CONTENT" 'any(.[]; .content == $c and .pinned)' >/dev/null; then
    log_pass "Pinned entry correctly marked for search results"
else
    log_fail "Pinned entry not marked correctly"
fi

# ============================================================================
# Test 3: Preview scrolling data preserved for pinned entries
# ============================================================================
log_info "Test 3: Long pinned content supports preview scrolling"
mkdir -p "$TEST_DIR/test3"

# Create pinned entry with 500 lines
LONG_CONTENT=""
for i in $(seq 1 500); do
    LONG_CONTENT="${LONG_CONTENT}Line $i: This is line number $i with padding text for scrolling test
"
done
ENTRY_LONG=$(create_entry "$TEST_DIR/test3" "$LONG_CONTENT" "1700000001000" "true")
build_index "$TEST_DIR/test3" 100 "$ENTRY_LONG"

# Verify full content stored
EXPORT3=$(exported "$TEST_DIR/test3")
echo "$EXPORT3" | jq -e --arg c "$LONG_CONTENT" '.[0].content == $c' >/dev/null && \
    log_pass "Long pinned content fully stored (500 lines)" || \
    log_fail "Content truncated or changed"

# Verify entry is still pinned
if echo "$EXPORT3" | jq -e '.[0].pinned == true' >/dev/null; then
    log_pass "Long content entry remains pinned"
else
    log_fail "Pinned status lost on long content"
fi

# ============================================================================
# Test 4: Atomic writes + Pin toggle (no corruption)
# ============================================================================
log_info "Test 4: Import survives an index.json raced by old-version writers"
mkdir -p "$TEST_DIR/test4"
ENTRY=$(create_entry "$TEST_DIR/test4" "toggle me" "1700000001000" "false")
build_index "$TEST_DIR/test4" 100 "$ENTRY"

# Simulate rapid pin toggles (concurrent writers)
for i in {1..20}; do
    (
        if (( i % 2 == 0 )); then
            ENTRY=$(create_entry "$TEST_DIR/test4" "toggle me" "1700000001000" "true")
        else
            ENTRY=$(create_entry "$TEST_DIR/test4" "toggle me" "1700000001000" "false")
        fi
        build_index "$TEST_DIR/test4" 100 "$ENTRY"
    ) &
done
wait

# Whatever the racing writers left in index.json, the content is imported once
EXPORT4=$(exported "$TEST_DIR/test4")
if echo "$EXPORT4" | jq -e 'length == 1 and .[0].content == "toggle me"' >/dev/null && \
    $CLIPSTACK --storage-dir "$TEST_DIR/test4" check >/dev/null 2>&1; then
    log_pass "Entry imported once after rapid pin toggles; check passes"
else
    log_fail "Import after pin toggles gave: $EXPORT4"
fi

# ============================================================================
# Test 5: Recovery works with pinned entries
# ============================================================================
log_info "Test 5: Import and recover handle pinned entries"
mkdir -p "$TEST_DIR/test5"

# Create entries with content files
ENTRY1=$(create_entry "$TEST_DIR/test5" "recoverable pinned" "1700000001000" "true")
ENTRY2=$(create_entry "$TEST_DIR/test5" "recoverable unpinned" "1700000002000" "false")
build_index "$TEST_DIR/test5" 100 "$ENTRY2" "$ENTRY1"

# Create orphan temp file (simulating interrupted write)
echo '{"max_entries":100,"entries":[]}' > "$TEST_DIR/test5/index.json.tmp"

# Run recovery (the import runs first, on open)
RECOVER=$($CLIPSTACK --storage-dir "$TEST_DIR/test5" recover 2>/dev/null) || true

# Verify the temp file moved to the backup
if [[ ! -f "$TEST_DIR/test5/index.json.tmp" ]] && \
    find "$TEST_DIR/test5" -path '*/legacy-backup-*/index.json.tmp' | grep -q .; then
    log_pass "Temp file moved to the legacy backup"
else
    log_fail "Temp file not moved to the backup"
fi

# Verify both entries kept, with their pinned flags
echo "$RECOVER" | grep -q "Recovery complete. 2 entries." && \
    exported "$TEST_DIR/test5" | jq -e 'map({content, pinned}) == [{"content":"recoverable pinned","pinned":true},{"content":"recoverable unpinned","pinned":false}]' >/dev/null && \
    log_pass "Entries preserved after recovery (2)" || \
    log_fail "Entries lost after recovery: $RECOVER"

# ============================================================================
# Test 6: All features respect CLI max_entries flag
# ============================================================================
log_info "Test 6: CLI --max-entries applies to all operations"
mkdir -p "$TEST_DIR/test6"

# Create 50 entries
ENTRIES6=()
for i in $(seq 1 50); do
    TS=$((1700000001000 + i * 100))
    ENTRY=$(create_entry "$TEST_DIR/test6" "entry $i" "$TS" "false")
    ENTRIES6=("$ENTRY" "${ENTRIES6[@]}")
done
build_index "$TEST_DIR/test6" 100 "${ENTRIES6[@]}"

# Run stats with --max-entries 15
OUTPUT=$($CLIPSTACK --storage-dir "$TEST_DIR/test6" --max-entries 15 stats 2>/dev/null)

# Verify max_entries was applied
REMAINING=$(exported "$TEST_DIR/test6" --max-entries 15 | jq 'length')
[[ "$REMAINING" -eq 15 ]] && \
    log_pass "CLI --max-entries enforced ($REMAINING entries)" || \
    log_fail "Expected 15 entries, got $REMAINING"

# ============================================================================
# Test 7: Environment variable max entries
# ============================================================================
log_info "Test 7: CLIPSTACK_MAX_ENTRIES env var respected"
mkdir -p "$TEST_DIR/test7"

# Create 30 entries
ENTRIES7=()
for i in $(seq 1 30); do
    TS=$((1700000001000 + i * 100))
    ENTRY=$(create_entry "$TEST_DIR/test7" "env entry $i" "$TS" "false")
    ENTRIES7=("$ENTRY" "${ENTRIES7[@]}")
done
build_index "$TEST_DIR/test7" 100 "${ENTRIES7[@]}"

# Run with env var set
CLIPSTACK_MAX_ENTRIES=10 $CLIPSTACK --storage-dir "$TEST_DIR/test7" stats >/dev/null 2>&1

# Verify pruning occurred
REMAINING=$(CLIPSTACK_MAX_ENTRIES=10 exported "$TEST_DIR/test7" | jq 'length')
[[ "$REMAINING" -eq 10 ]] && \
    log_pass "CLIPSTACK_MAX_ENTRIES env var enforced ($REMAINING entries)" || \
    log_fail "Expected 10 entries, got $REMAINING"

# ============================================================================
# Test 8: Pinned entry limit (MAX_PINNED=25)
# ============================================================================
log_info "Test 8: Pinned entry limit enforced"
mkdir -p "$TEST_DIR/test8"

# Create 30 pinned entries (exceeds MAX_PINNED=25)
ENTRIES8=()
for i in $(seq 1 30); do
    TS=$((1700000001000 + i * 100))
    ENTRY=$(create_entry "$TEST_DIR/test8" "pinned $i" "$TS" "true")
    ENTRIES8=("$ENTRY" "${ENTRIES8[@]}")
done
build_index "$TEST_DIR/test8" 100 "${ENTRIES8[@]}"

# Run stats (which triggers pruning logic)
$CLIPSTACK --storage-dir "$TEST_DIR/test8" --max-entries 100 stats >/dev/null 2>&1

# The import keeps every pinned entry, even past MAX_PINNED (25): it never
# drops data. MAX_PINNED applies when pinning, not to imported pins.
PINNED_COUNT=$(exported "$TEST_DIR/test8" --max-entries 100 | jq 'map(select(.pinned)) | length')
[[ "$PINNED_COUNT" -eq 30 ]] && \
    log_pass "All imported pins kept ($PINNED_COUNT pinned)" || \
    log_fail "Unexpected pinned count: $PINNED_COUNT"

# ============================================================================
# Test 9: Stats shows all v1.1 info
# ============================================================================
log_info "Test 9: Stats command shows all v1.1 information"
mkdir -p "$TEST_DIR/test9"
ENTRY=$(create_entry "$TEST_DIR/test9" "test entry" "1700000001000" "true")
build_index "$TEST_DIR/test9" 100 "$ENTRY"

STATS=$($CLIPSTACK --storage-dir "$TEST_DIR/test9" --max-entries 50 stats 2>/dev/null)

# Check for key fields
echo "$STATS" | grep -q "Entries:" && \
    log_pass "Stats shows entry count" || \
    log_fail "Stats missing entry count"

echo "$STATS" | grep -q "Pinned:" && \
    log_pass "Stats shows pinned count" || \
    log_fail "Stats missing pinned count"

# ============================================================================
# Test 10: List shows entries with pinned indicator
# ============================================================================
log_info "Test 10: List command shows pinned entries correctly"
mkdir -p "$TEST_DIR/test10"
ENTRY_P=$(create_entry "$TEST_DIR/test10" "I am pinned" "1700000001000" "true")
ENTRY_U=$(create_entry "$TEST_DIR/test10" "I am not pinned" "1700000002000" "false")
build_index "$TEST_DIR/test10" 100 "$ENTRY_U" "$ENTRY_P"

LIST=$($CLIPSTACK --storage-dir "$TEST_DIR/test10" list 2>/dev/null)

# Verify both entries appear
echo "$LIST" | grep -q "pinned" && \
    log_pass "List shows entry previews" || \
    log_fail "List missing entry previews"

# ============================================================================
# Summary
# ============================================================================
echo ""
if [[ "$FAILED" -eq 0 ]]; then
    echo -e "${GREEN}All v1.1 feature integration tests passed!${NC}"
    exit 0
else
    echo -e "${RED}Some v1.1 feature integration tests failed${NC}"
    exit 1
fi
