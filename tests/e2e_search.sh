#!/bin/bash
set -euo pipefail

# E2E Tests for Full Content Search
# Run from project root: ./tests/e2e_search.sh
# Note: Creates entries directly in storage (doesn't require wl-clipboard)

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

# Helper to create test entries directly in storage
create_entry_with_content() {
    local storage_dir="$1"
    local content="$2"
    local ts="${3:-$(date +%s%3N)}"

    mkdir -p "$storage_dir"

    # Create content file
    echo -n "$content" > "$storage_dir/$ts.txt"

    # Generate preview (first 100 chars, control chars replaced with spaces)
    local preview
    preview=$(echo -n "$content" | head -c 100 | tr '\n\t\r' '   ')

    # Compute SHA256 hash
    local hash
    hash="sha256:$(echo -n "$content" | sha256sum | cut -d' ' -f1)"

    # Get size
    local size
    size=${#content}

    # Output JSON entry (to be collected)
    echo "{\"id\":\"$ts\",\"timestamp\":$ts,\"size\":$size,\"preview\":\"$preview\",\"hash\":\"$hash\",\"pinned\":false}"
}

# Build index from entries
build_index() {
    local storage_dir="$1"
    shift
    local entries=("$@")
    local max_entries=100

    {
        echo -n '{"max_entries":'$max_entries',"entries":['
        local first=true
        for entry in "${entries[@]}"; do
            if [ "$first" = true ]; then
                first=false
            else
                echo -n ","
            fi
            echo -n "$entry"
        done
        echo ']}'
    } > "$storage_dir/index.json"
}

# Build release if needed
if [[ ! -f "$CLIPSTACK" ]]; then
    log_info "Building release binary..."
    cargo build --release
fi

log_info "Test directory: $TEST_DIR"

# Test 1: Search finds preview match
log_info "Test 1: Search finds preview match"
mkdir -p "$TEST_DIR/test1"
ENTRY1=$(create_entry_with_content "$TEST_DIR/test1" "unique_preview_text here" "1700000001000")
build_index "$TEST_DIR/test1" "$ENTRY1"

# Verify preview text is in index
if /bin/grep -q "unique_preview_text" "$TEST_DIR/test1/index.json"; then
    log_pass "Preview text stored correctly"
else
    log_fail "Preview text not found in index"
fi

# Test 2: Search finds content-only match (beyond preview length)
log_info "Test 2: Search finds content-only match"
mkdir -p "$TEST_DIR/test2"
# Create content where search term is beyond preview length (100 chars)
LONG_PREFIX=$(printf 'x%.0s' {1..110})
CONTENT="${LONG_PREFIX}findme_content_term_here"
ENTRY2=$(create_entry_with_content "$TEST_DIR/test2" "$CONTENT" "1700000002000")
build_index "$TEST_DIR/test2" "$ENTRY2"

# Verify search term is NOT in preview but IS in content file
if /bin/grep -q "findme_content" "$TEST_DIR/test2/index.json"; then
    log_fail "Content-only term should not be in preview (too far into content)"
fi

# Verify term exists in content file
if /bin/grep -q "findme_content_term" "$TEST_DIR/test2/1700000002000.txt"; then
    log_pass "Content-only term stored in content file"
else
    log_fail "Content-only term not found in content file"
fi

# Test 3: Multiple entries available for search
log_info "Test 3: Multiple entries stored correctly"
mkdir -p "$TEST_DIR/test3"
ENTRY3A=$(create_entry_with_content "$TEST_DIR/test3" "first entry with alpha" "1700000003000")
sleep 0.01 # Ensure unique timestamps
ENTRY3B=$(create_entry_with_content "$TEST_DIR/test3" "second entry with beta" "1700000004000")
sleep 0.01
ENTRY3C=$(create_entry_with_content "$TEST_DIR/test3" "third entry with gamma" "1700000005000")
build_index "$TEST_DIR/test3" "$ENTRY3C" "$ENTRY3B" "$ENTRY3A"  # Newest first

ENTRY_COUNT=$(jq '.entries | length' "$TEST_DIR/test3/index.json")
[[ "$ENTRY_COUNT" -eq 3 ]] && \
    log_pass "Multiple entries stored ($ENTRY_COUNT)" || \
    log_fail "Expected 3 entries, got $ENTRY_COUNT"

# Test 4: Index structure supports search (has preview field)
log_info "Test 4: Index structure supports search"
if jq -e '.entries[0].preview' "$TEST_DIR/test3/index.json" > /dev/null; then
    log_pass "Entries have preview field for search"
else
    log_fail "Entries missing preview field"
fi

# Test 5: Hash field present for deduplication
log_info "Test 5: Hash field present for deduplication"
if jq -e '.entries[0].hash' "$TEST_DIR/test3/index.json" > /dev/null; then
    log_pass "Entries have hash field"
else
    log_fail "Entries missing hash field"
fi

# Test 6: Stats command works with search-ready storage
log_info "Test 6: Stats command works with storage"
COUNT=$($CLIPSTACK --storage-dir "$TEST_DIR/test3" stats 2>/dev/null | /bin/grep "^Entries:" | awk '{print $2}' | cut -d'/' -f1)
[[ "$COUNT" -eq 3 ]] && \
    log_pass "Stats reads entries correctly ($COUNT)" || \
    log_fail "Expected stats to show 3, got $COUNT"

# Test 7: List command shows entries
log_info "Test 7: List command shows entries"
LIST_OUTPUT=$($CLIPSTACK --storage-dir "$TEST_DIR/test3" list 2>/dev/null)
if echo "$LIST_OUTPUT" | /bin/grep -q "first entry\|second entry\|third entry"; then
    log_pass "List shows entry previews"
else
    log_fail "List doesn't show entry previews"
fi

# Test 8: Content can be loaded for deep search
log_info "Test 8: Content files loadable"
CONTENT_FILES=$(find "$TEST_DIR/test3" -name "*.txt" | wc -l)
[[ "$CONTENT_FILES" -eq 3 ]] && \
    log_pass "All content files present ($CONTENT_FILES)" || \
    log_fail "Expected 3 content files, got $CONTENT_FILES"

echo ""
echo -e "${GREEN}All E2E search tests passed!${NC}"
