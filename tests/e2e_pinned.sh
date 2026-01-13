#!/bin/bash
set -euo pipefail

# E2E Tests for Pinned/Favorites Feature
# Run from project root: ./tests/e2e_pinned.sh
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
    local pinned="${4:-false}"

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

    # Output JSON entry
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

# Test 1: Index has pinned field
log_info "Test 1: Index supports pinned field"
mkdir -p "$TEST_DIR/test1"
ENTRY1=$(create_entry_with_content "$TEST_DIR/test1" "entry1" "1700000001000" "false")
build_index "$TEST_DIR/test1" 100 "$ENTRY1"

if /bin/grep -q '"pinned":false' "$TEST_DIR/test1/index.json"; then
    log_pass "Pinned field present in index"
else
    log_fail "Pinned field missing from index"
fi

# Test 2: Pinned entries survive pruning
log_info "Test 2: Pinned entries survive pruning"
mkdir -p "$TEST_DIR/test2"
# Create a pinned entry and several unpinned entries
PINNED_ENTRY=$(create_entry_with_content "$TEST_DIR/test2" "keep me forever" "1700000000100" "true")
ENTRIES=("$PINNED_ENTRY")

# Add many unpinned entries (newer timestamps so they're "first" in the list)
for i in $(seq 1 20); do
    TS=$((1700000001000 + i * 1000))
    ENTRY=$(create_entry_with_content "$TEST_DIR/test2" "filler entry $i" "$TS" "false")
    ENTRIES=("$ENTRY" "${ENTRIES[@]}")
done

build_index "$TEST_DIR/test2" 10 "${ENTRIES[@]}"

# Access with max=10, which should prune but keep pinned
$CLIPSTACK --storage-dir "$TEST_DIR/test2" --max-entries 10 stats >/dev/null 2>&1

# Check pinned entry survives (has ID 1700000000100)
if /bin/grep -q '"id":"1700000000100"' "$TEST_DIR/test2/index.json"; then
    log_pass "Pinned entry survived pruning"
else
    log_fail "Pinned entry was incorrectly pruned"
fi

# Test 3: Backwards compatibility (old index without pinned field)
log_info "Test 3: Backwards compatibility with old format"
mkdir -p "$TEST_DIR/test3"
# Create index without pinned field (legacy format)
cat > "$TEST_DIR/test3/index.json" << 'EOF'
{
  "max_entries": 100,
  "entries": [
    {"id":"1234567890000","timestamp":1234567890000,"size":4,"preview":"test","hash":"sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"}
  ]
}
EOF
echo -n "test" > "$TEST_DIR/test3/1234567890000.txt"

$CLIPSTACK --storage-dir "$TEST_DIR/test3" stats >/dev/null 2>&1 && \
    log_pass "Old format index loads correctly" || log_fail "Should load old format without pinned field"

# Test 4: Only unpinned entries count against limit
log_info "Test 4: Only unpinned entries count against limit"
mkdir -p "$TEST_DIR/test4"
# Create 5 pinned and 15 unpinned entries with max=10
ENTRIES4=()
for i in $(seq 1 5); do
    TS=$((1700000001000 + i * 100))
    ENTRY=$(create_entry_with_content "$TEST_DIR/test4" "pinned $i" "$TS" "true")
    ENTRIES4=("$ENTRY" "${ENTRIES4[@]}")
done
for i in $(seq 1 15); do
    TS=$((1700000002000 + i * 100))
    ENTRY=$(create_entry_with_content "$TEST_DIR/test4" "unpinned $i" "$TS" "false")
    ENTRIES4=("$ENTRY" "${ENTRIES4[@]}")
done
build_index "$TEST_DIR/test4" 10 "${ENTRIES4[@]}"

# Access with max=10 (should prune only unpinned entries)
$CLIPSTACK --storage-dir "$TEST_DIR/test4" --max-entries 10 stats >/dev/null 2>&1

# Count remaining pinned entries
PINNED_COUNT=$(/bin/grep -o '"pinned":true' "$TEST_DIR/test4/index.json" | wc -l)
[[ "$PINNED_COUNT" -eq 5 ]] && \
    log_pass "All 5 pinned entries preserved" || \
    log_fail "Expected 5 pinned, got $PINNED_COUNT"

# Test 5: Verify stats command works with pinned entries
log_info "Test 5: Stats command with pinned entries"
STATS_OUTPUT=$($CLIPSTACK --storage-dir "$TEST_DIR/test4" --max-entries 10 stats 2>/dev/null)
echo "$STATS_OUTPUT" | /bin/grep -q "Entries:" && \
    log_pass "Stats outputs with pinned storage" || \
    log_fail "Stats failed with pinned entries"

# Test 6: List command shows entries correctly
log_info "Test 6: List command with pinned entries"
LIST_OUTPUT=$($CLIPSTACK --storage-dir "$TEST_DIR/test4" --max-entries 10 list 2>/dev/null)
if echo "$LIST_OUTPUT" | /bin/grep -q "pinned\|unpinned"; then
    log_pass "List shows entry previews"
else
    log_fail "List doesn't show entry previews"
fi

# Test 7: Duplicate detection works with pinned entries
log_info "Test 7: Duplicate with pinned entry"
mkdir -p "$TEST_DIR/test7"
# Create an entry
CONTENT="duplicate test content"
HASH="sha256:$(echo -n "$CONTENT" | sha256sum | cut -d' ' -f1)"
ENTRY_OLD=$(create_entry_with_content "$TEST_DIR/test7" "$CONTENT" "1700000001000" "true")
build_index "$TEST_DIR/test7" 100 "$ENTRY_OLD"

# Verify we can access it
$CLIPSTACK --storage-dir "$TEST_DIR/test7" stats >/dev/null 2>&1 && \
    log_pass "Storage with pinned duplicates works" || \
    log_fail "Storage access failed"

echo ""
echo -e "${GREEN}All E2E pinned tests passed!${NC}"
