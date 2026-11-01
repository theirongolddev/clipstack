#!/bin/bash
set -euo pipefail

# E2E Tests for storage integrity: legacy import, backup, check, permissions
# Run from project root: ./tests/e2e_atomic_writes.sh
# (The name is historical: this suite used to test the JSON store's atomic
# writes and recovery. The SQLite store replaced both.)
#
# Note: Creates legacy index.json + content-file stores directly (doesn't
# require wl-clipboard); every command imports them into SQLite.

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Temp XDG dirs and HOME, fake wl-paste/wl-copy first on PATH, no WAYLAND_DISPLAY
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
isolate_env

TEST_DIR="$ISOLATED_ROOT/work"
mkdir -p "$TEST_DIR"
CLIPSTACK="./target/release/clipstack"
GREP="/usr/bin/grep"

log_pass() { echo -e "${GREEN}✓${NC} $1"; }
log_fail() { echo -e "${RED}✗${NC} $1"; exit 1; }
log_info() { echo -e "${YELLOW}→${NC} $1"; }

# cleanup (tests/lib.sh) stops only tracked PIDs and removes the temp root
trap cleanup EXIT

# Helper to create a legacy store (index.json + <ts>.txt files)
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

# Entry count from `stats` ("Entries:     N")
entry_count() {
    $CLIPSTACK --storage-dir "$1" "${@:2}" stats 2>/dev/null | $GREP "^Entries:" | awk '{print $2}'
}

# The only legacy-backup-* dir in a store (fails if there isn't exactly one)
backup_dir() {
    local dirs
    dirs=$(find "$1" -maxdepth 1 -type d -name 'legacy-backup-*')
    [[ $(echo "$dirs" | $GREP -c .) -eq 1 ]] || return 1
    echo "$dirs"
}

mode_of() { stat -c '%a' "$1"; }

# Test 1: Import moves the legacy files into one backup dir
log_info "Test 1: Legacy import moves index.json, content and .tmp files to a backup"
create_test_entries "$TEST_DIR/test1" 5 100
touch "$TEST_DIR/test1/index.tmp" "$TEST_DIR/test1/orphan.tmp"
COUNT=$(entry_count "$TEST_DIR/test1")
[[ "$COUNT" -eq 5 ]] && \
    log_pass "Imported $COUNT entries" || \
    log_fail "Expected 5 imported entries, got $COUNT"
LEFT=$(find "$TEST_DIR/test1" -maxdepth 1 -type f \( -name '*.txt' -o -name '*.tmp' -o -name 'index.json' \) | wc -l)
[[ "$LEFT" -eq 0 ]] && \
    log_pass "No legacy files left in the store root" || \
    log_fail "Found $LEFT legacy files left in the store root"
BACKUP=$(backup_dir "$TEST_DIR/test1") || log_fail "Expected exactly one legacy-backup-* dir"
MOVED=$(find "$BACKUP" -type f | wc -l)
[[ "$MOVED" -eq 8 && -f "$BACKUP/index.json" ]] && \
    log_pass "Backup holds index.json, 5 content files and 2 .tmp files" || \
    log_fail "Expected 8 files in $BACKUP, got $MOVED"
[[ "$(mode_of "$BACKUP")" == "700" ]] && \
    log_pass "Backup dir is 0700" || \
    log_fail "Backup dir mode is $(mode_of "$BACKUP")"

# Test 2: Import is one-time; contents match the legacy files
log_info "Test 2: Import is one-time and keeps content byte-exact"
STDERR=$($CLIPSTACK --storage-dir "$TEST_DIR/test1" stats 2>&1 >/dev/null)
echo "$STDERR" | $GREP -q "Imported" && log_fail "Second command imported again: $STDERR"
log_pass "No import notice on the second command"
# create_test_entries writes with echo, so each file ends with a newline
$CLIPSTACK --storage-dir "$TEST_DIR/test1" export | \
    jq -e -s 'length == 5 and (map(.content) == ["test entry 1\n","test entry 2\n","test entry 3\n","test entry 4\n","test entry 5\n"])' >/dev/null && \
    log_pass "Exported contents match the legacy files, oldest first" || \
    log_fail "Exported contents differ from the legacy files"

# Test 3: Content files without index.json are imported
log_info "Test 3: Orphaned content files are imported without an index"
mkdir -p "$TEST_DIR/test3"
echo "orphan content 1" > "$TEST_DIR/test3/1700000001000.txt"
echo "orphan content 2" > "$TEST_DIR/test3/1700000002000.txt"
echo "orphan content 3" > "$TEST_DIR/test3/1700000003000.txt"
COUNT=$(entry_count "$TEST_DIR/test3")
[[ "$COUNT" -eq 3 ]] && \
    log_pass "Imported $COUNT orphaned content files" || \
    log_fail "Expected 3 entries from orphaned files, got $COUNT"

# Test 4: An unreadable index.json doesn't stop the import
log_info "Test 4: Corrupted index.json"
mkdir -p "$TEST_DIR/test4"
echo "valid content file" > "$TEST_DIR/test4/1700000004000.txt"
echo "{ this is not valid json ]]{{" > "$TEST_DIR/test4/index.json"
STDERR=$($CLIPSTACK --storage-dir "$TEST_DIR/test4" stats 2>&1 >/dev/null) || \
    log_fail "stats failed on a corrupted index.json"
echo "$STDERR" | $GREP -q "unreadable" && \
    log_pass "Warned about the unreadable index.json" || \
    log_fail "No warning about the unreadable index.json: $STDERR"
COUNT=$(entry_count "$TEST_DIR/test4")
[[ "$COUNT" -eq 1 ]] && \
    log_pass "Content file imported despite the bad index" || \
    log_fail "Expected 1 entry, got $COUNT"

# Test 5: Identical content is stored once
log_info "Test 5: Import deduplicates identical content"
mkdir -p "$TEST_DIR/test5"
echo "duplicate content" > "$TEST_DIR/test5/1700000005000.txt"
echo "duplicate content" > "$TEST_DIR/test5/1700000006000.txt"
echo "duplicate content" > "$TEST_DIR/test5/1700000007000.txt"
COUNT=$(entry_count "$TEST_DIR/test5")
[[ "$COUNT" -eq 1 ]] && \
    log_pass "Deduplicated to $COUNT entry" || \
    log_fail "Expected 1 entry after dedup, got $COUNT"
$CLIPSTACK --storage-dir "$TEST_DIR/test5" export | \
    jq -e -s '.[0].created_at == 1700000005000 and .[0].last_used == 1700000007000' >/dev/null && \
    log_pass "Merged entry keeps the first and last timestamps" || \
    log_fail "Merged entry has the wrong timestamps"

# Test 6: check passes after pruning; recover still works for old scripts
log_info "Test 6: check after pruning, and the recover alias"
create_test_entries "$TEST_DIR/test6" 50 100
COUNT=$(entry_count "$TEST_DIR/test6" --max-entries 10)
[[ "$COUNT" -eq 10 ]] && \
    log_pass "Pruned to $COUNT entries" || \
    log_fail "Expected 10 entries after pruning, got $COUNT"
CHECK=$($CLIPSTACK --storage-dir "$TEST_DIR/test6" check) || log_fail "check failed: $CHECK"
echo "$CHECK" | $GREP -q "^Integrity:    ok" && echo "$CHECK" | $GREP -q "^Search index: ok" && \
    log_pass "check reports integrity and search index ok" || \
    log_fail "Unexpected check output: $CHECK"
echo "$CHECK" | $GREP -q "^Legacy backup: .*legacy-backup-" && \
    log_pass "check lists the legacy backup" || \
    log_fail "check doesn't list the legacy backup: $CHECK"
$CLIPSTACK --storage-dir "$TEST_DIR/test6" check --repair >/dev/null || log_fail "check --repair failed"
$CLIPSTACK --storage-dir "$TEST_DIR/test6" recover | $GREP -q "Recovery complete. 10 entries." && \
    log_pass "recover alias prints Recovery complete" || \
    log_fail "recover alias output changed"

# Test 7: Files an old daemon writes after the import are merged, not duplicated
log_info "Test 7: Legacy files reappearing after the import are merged"
for i in $(seq 1 3); do
    create_test_entries "$TEST_DIR/test7" "$((i * 5))" 100
    $CLIPSTACK --storage-dir "$TEST_DIR/test7" stats >/dev/null 2>&1
done
COUNT=$(entry_count "$TEST_DIR/test7")
BACKUPS=$(find "$TEST_DIR/test7" -maxdepth 1 -type d -name 'legacy-backup-*' | wc -l)
[[ "$COUNT" -eq 15 && "$BACKUPS" -eq 3 ]] && \
    log_pass "15 distinct entries after 3 imports, one backup dir each" || \
    log_fail "Expected 15 entries and 3 backups, got $COUNT and $BACKUPS"

# Test 8: Database files are private
log_info "Test 8: Database permissions"
DB="$TEST_DIR/test7/clipstack.db"
[[ "$(mode_of "$TEST_DIR/test7")" == "700" ]] && \
    log_pass "Store dir is 0700" || \
    log_fail "Store dir mode is $(mode_of "$TEST_DIR/test7")"
for f in "$DB" "$DB-wal" "$DB-shm"; do
    [[ ! -e "$f" || "$(mode_of "$f")" == "600" ]] || log_fail "$(basename "$f") mode is $(mode_of "$f")"
done
[[ -f "$DB" ]] && log_pass "clipstack.db and its WAL/SHM files are 0600" || log_fail "clipstack.db missing"

echo ""
echo -e "${GREEN}All E2E storage integrity tests passed!${NC}"
