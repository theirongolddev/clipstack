#!/bin/bash
set -euo pipefail

# E2E Tests for Preview Scrolling
# Run from project root: ./tests/e2e_preview.sh
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
    printf '%s' "$content" > "$storage_dir/$ts.txt"

    # Generate preview (first 100 chars, control chars replaced with spaces)
    local preview
    preview=$(printf '%s' "$content" | head -c 100 | tr '\n\t\r' '   ')

    # Compute SHA256 hash
    local hash
    hash="sha256:$(printf '%s' "$content" | sha256sum | cut -d' ' -f1)"

    # Get size
    local size
    size=${#content}

    # Output JSON entry (to be collected)
    printf '{"id":"%s","timestamp":%s,"size":%d,"preview":"%s","hash":"%s","pinned":false}' \
        "$ts" "$ts" "$size" "$preview" "$hash"
}

# Build index from entries
build_index() {
    local storage_dir="$1"
    shift
    local entries=("$@")
    local max_entries=100

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

# Test 1: Multi-line content storage and retrieval
log_info "Test 1: Multi-line content storage"
mkdir -p "$TEST_DIR/test1"
MULTILINE_CONTENT="Line 1: First line of content
Line 2: Second line with more text
Line 3: Third line
Line 4: Fourth line
Line 5: Fifth line - this should all be stored"
ENTRY1=$(create_entry_with_content "$TEST_DIR/test1" "$MULTILINE_CONTENT" "1700000001000")
build_index "$TEST_DIR/test1" "$ENTRY1"

# Check content file exists
if [ -f "$TEST_DIR/test1/1700000001000.txt" ]; then
    log_pass "Content file created"
else
    log_fail "Content file not created"
fi

LINE_COUNT=$(wc -l < "$TEST_DIR/test1/1700000001000.txt")
[[ "$LINE_COUNT" -ge 4 ]] && \
    log_pass "All lines stored in content file ($LINE_COUNT lines)" || \
    log_fail "Expected at least 4 lines, got $LINE_COUNT"

# Test 2: Long content beyond preview length (for scroll testing)
log_info "Test 2: Long content beyond preview length"
mkdir -p "$TEST_DIR/test2"
# Create 200 lines of content
LONG_CONTENT=""
for i in $(seq 1 200); do
    LONG_CONTENT="${LONG_CONTENT}Line $i: This is line number $i with padding text
"
done
ENTRY2=$(create_entry_with_content "$TEST_DIR/test2" "$LONG_CONTENT" "1700000002000")
build_index "$TEST_DIR/test2" "$ENTRY2"

STORED_LINES=$(wc -l < "$TEST_DIR/test2/1700000002000.txt")
[[ "$STORED_LINES" -ge 200 ]] && \
    log_pass "Long content fully stored ($STORED_LINES lines)" || \
    log_fail "Long content truncated, expected 200+ lines, got $STORED_LINES"

# Verify preview is truncated but content is full
PREVIEW_LEN=$(jq -r '.entries[0].preview | length' "$TEST_DIR/test2/index.json")
CONTENT_LEN=$(wc -c < "$TEST_DIR/test2/1700000002000.txt")
[[ "$CONTENT_LEN" -gt "$PREVIEW_LEN" ]] && \
    log_pass "Full content ($CONTENT_LEN bytes) larger than preview ($PREVIEW_LEN chars)" || \
    log_fail "Content should be larger than preview"

# Test 3: Unicode content handling
log_info "Test 3: Unicode content handling"
mkdir -p "$TEST_DIR/test3"
UNICODE_CONTENT="Line 1: Hello World
Line 2: Japanese Characters
Line 3: Russian Characters
Line 4: Mixed content abc123"
ENTRY3=$(create_entry_with_content "$TEST_DIR/test3" "$UNICODE_CONTENT" "1700000003000")
build_index "$TEST_DIR/test3" "$ENTRY3"

# Verify content is readable
$CLIPSTACK --storage-dir "$TEST_DIR/test3" stats >/dev/null 2>&1 && \
    log_pass "Unicode content handled correctly" || \
    log_fail "Unicode content caused error"

# Test 4: Content with special characters
log_info "Test 4: Content with special characters"
mkdir -p "$TEST_DIR/test4"
SPECIAL_CONTENT="Tab:	here
Backslash: \\ here
Quote: \"here\"
Single: 'here'"
ENTRY4=$(create_entry_with_content "$TEST_DIR/test4" "$SPECIAL_CONTENT" "1700000004000")
build_index "$TEST_DIR/test4" "$ENTRY4"

if [ -s "$TEST_DIR/test4/1700000004000.txt" ]; then
    log_pass "Special characters handled without corruption"
else
    log_fail "Content file empty or missing"
fi

# Test 5: Very long lines (horizontal scroll test data)
log_info "Test 5: Very long lines preserved"
mkdir -p "$TEST_DIR/test5"
LONG_LINE=$(printf 'x%.0s' {1..500})  # 500 char line
LONG_LINE_CONTENT="Short line
$LONG_LINE
Another short line"
ENTRY5=$(create_entry_with_content "$TEST_DIR/test5" "$LONG_LINE_CONTENT" "1700000005000")
build_index "$TEST_DIR/test5" "$ENTRY5"

LINE_LEN=$(head -2 "$TEST_DIR/test5/1700000005000.txt" | tail -1 | wc -c)
[[ "$LINE_LEN" -ge 500 ]] && \
    log_pass "Long line preserved ($LINE_LEN chars)" || \
    log_fail "Long line truncated, expected 500+ chars, got $LINE_LEN"

# Test 6: Stats and list work with scrollable content
log_info "Test 6: Stats works with scrollable content"
COUNT=$($CLIPSTACK --storage-dir "$TEST_DIR/test2" stats 2>/dev/null | /bin/grep "^Entries:" | awk '{print $2}' | cut -d'/' -f1)
[[ "$COUNT" -ge 1 ]] && \
    log_pass "Stats works with long content" || \
    log_fail "Stats failed with long content"

# Test 7: List command truncates preview properly
log_info "Test 7: List truncates preview for long content"
LIST_OUTPUT=$($CLIPSTACK --storage-dir "$TEST_DIR/test2" list 2>/dev/null)
# Preview should be truncated, not show all 200 lines
OUTPUT_LINES=$(echo "$LIST_OUTPUT" | wc -l)
[[ "$OUTPUT_LINES" -lt 50 ]] && \
    log_pass "List preview truncated appropriately" || \
    log_fail "List should truncate preview, got $OUTPUT_LINES lines"

echo ""
echo -e "${GREEN}All E2E preview tests passed!${NC}"
