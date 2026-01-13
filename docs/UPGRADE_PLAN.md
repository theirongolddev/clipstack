# ClipStack Upgrade Plan

> A strategic roadmap for making ClipStack more robust, reliable, and user-friendly.

## Executive Summary

This document outlines five high-impact improvements to ClipStack, prioritized by value-to-effort ratio. Each improvement addresses a real user need or reliability concern while remaining pragmatic and well-scoped. The improvements are designed to be implemented incrementally without requiring major architectural changes.

### Improvement Overview

| Priority | Improvement | Category | Effort | Impact |
|----------|-------------|----------|--------|--------|
| 1 | Full Content Search | Usability | Medium | High |
| 2 | Pinned/Favorites | Feature | Low | High |
| 3 | Atomic File Writes | Reliability | Low | Critical |
| 4 | Preview Scrolling | UX | Low | Medium |
| 5 | Configurable Max Entries | Customization | Very Low | Medium |

### Design Principles

All improvements in this plan adhere to these principles:

1. **Backwards Compatibility**: Existing storage format remains readable; new fields use `#[serde(default)]`
2. **Progressive Enhancement**: New features don't change existing workflows unless the user opts in
3. **Minimal Dependencies**: No new crate dependencies unless absolutely necessary
4. **Test-Driven**: Each improvement includes comprehensive tests before implementation

---

## Improvement #1: Full Content Search

### Problem Statement

Currently, ClipStack's fuzzy search only matches against the 100-character preview stored in `index.json`. This creates a significant usability gap:

```
User copies a 500-line code file
User wants to find it by searching for "handleAuthentication"
That function name appears on line 247
Search finds nothing because preview only contains lines 1-3
User cannot find their content
```

This limitation undermines the core value proposition of a clipboard history manager: **finding content you copied in the past**.

### Current Behavior

```rust
// picker.rs:95-115
fn update_filter(&mut self) {
    if self.search_query.is_empty() {
        self.filtered = (0..self.entries.len()).collect();
    } else {
        let mut scored: Vec<(usize, i64)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| {
                // BUG: Only searches preview, not full content
                self.matcher
                    .fuzzy_match(&entry.preview, &self.search_query)
                    .map(|score| (i, score))
            })
            .collect();
        // ...
    }
}
```

### Solution Design

Implement a two-phase search strategy:

1. **Phase 1 (Fast)**: Search previews first (already in memory)
2. **Phase 2 (Thorough)**: For non-matches, load and search full content on-demand

This approach balances performance with completeness. Most searches will find matches in previews (Phase 1 only). Only when preview search fails do we incur the I/O cost of loading full content.

### User Experience

- Search behavior remains unchanged for queries that match previews
- When a match is found in full content (not preview), show a visual indicator
- Results still sorted by relevance score
- No configuration required; this "just works"

**Visual Mockup**:
```
┌─Search (/ to search)──────────────────────────┐
│ handleAuth                                     │
└───────────────────────────────────────────────┘
┌─History (2/847) matching 'handleAuth'─────────┐
│> 2h ago [  12KB] import { auth } from...      │ ← Match in preview
│  5d ago [  45KB] /** API routes */ ...    [+] │ ← Match in content (indicator)
└───────────────────────────────────────────────┘
```

The `[+]` indicator shows this entry matched on full content, not preview.

### Implementation Details

#### File Changes

**`src/picker.rs`** - Modified search logic

```rust
/// Tracks whether a match was found in preview or full content
#[derive(Clone, Copy, PartialEq)]
enum MatchLocation {
    Preview,
    FullContent,
}

struct FilteredEntry {
    index: usize,
    score: i64,
    match_location: MatchLocation,
}

impl Picker {
    fn update_filter(&mut self) {
        if self.search_query.is_empty() {
            self.filtered = (0..self.entries.len())
                .map(|i| FilteredEntry {
                    index: i,
                    score: 0,
                    match_location: MatchLocation::Preview,
                })
                .collect();
            return;
        }

        let mut results: Vec<FilteredEntry> = Vec::new();
        let mut unmatched_indices: Vec<usize> = Vec::new();

        // Phase 1: Search previews (fast, in-memory)
        for (i, entry) in self.entries.iter().enumerate() {
            if let Some(score) = self.matcher.fuzzy_match(&entry.preview, &self.search_query) {
                results.push(FilteredEntry {
                    index: i,
                    score,
                    match_location: MatchLocation::Preview,
                });
            } else {
                unmatched_indices.push(i);
            }
        }

        // Phase 2: Search full content for non-matches (slower, requires I/O)
        // Only search if we have few preview matches and query is specific enough
        if results.len() < 10 && self.search_query.len() >= 2 {
            for i in unmatched_indices {
                let entry = &self.entries[i];
                if let Ok(content) = self.storage.load_content(&entry.id) {
                    if let Some(score) = self.matcher.fuzzy_match(&content, &self.search_query) {
                        results.push(FilteredEntry {
                            index: i,
                            // Slightly lower score for content-only matches
                            // so preview matches rank higher when scores are similar
                            score: score * 8 / 10,
                            match_location: MatchLocation::FullContent,
                        });
                    }
                }
            }
        }

        // Sort by score descending
        results.sort_by(|a, b| b.score.cmp(&a.score));
        self.filtered = results;

        // Update selection state
        if self.filtered.is_empty() {
            self.selected.select(None);
            self.preview_content = None;
            self.preview_id = None;
        } else {
            let current = self.selected.selected().unwrap_or(0);
            if current >= self.filtered.len() {
                self.selected.select(Some(0));
            }
        }
        self.update_scroll_state();
    }

    // Update render_list to show match location indicator
    fn render_list(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .map(|filtered| {
                let entry = &self.entries[filtered.index];
                let time = util::format_relative_time(entry.timestamp);
                let size = util::format_size(entry.size);

                let preview: String = entry
                    .preview
                    .chars()
                    .take(30)
                    .collect::<String>()
                    .replace('\n', " ");

                // Highlight matched characters if searching
                let preview_spans = if !self.search_query.is_empty() {
                    self.highlight_matches(&preview)
                } else {
                    vec![Span::raw(preview)]
                };

                let mut spans = vec![
                    Span::styled(
                        format!("{:>3} ", time),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("[{:>5}] ", size),
                        Style::default().fg(Color::Cyan),
                    ),
                ];
                spans.extend(preview_spans);

                // Add indicator for full-content matches
                if filtered.match_location == MatchLocation::FullContent {
                    spans.push(Span::styled(
                        " [+]",
                        Style::default().fg(Color::Yellow),
                    ));
                }

                ListItem::new(Line::from(spans))
            })
            .collect();

        // ... rest of render logic
    }
}
```

#### Performance Considerations

1. **Lazy Loading**: Full content is only loaded when preview search fails
2. **Early Termination**: Stop searching after finding 10 content matches
3. **Query Length Gate**: Only search full content for queries ≥2 characters
4. **Caching**: Consider caching recently-loaded content in memory (future optimization)

#### Alternative Approach: Pre-built Search Index

For users with large histories (500+ entries), on-demand content search may become slow. A future enhancement could pre-build a search index:

```rust
// Future enhancement - not part of initial implementation
pub struct SearchIndex {
    // Map of term -> list of (entry_id, positions)
    terms: HashMap<String, Vec<(String, Vec<usize>)>>,
}

impl Storage {
    pub fn build_search_index(&self) -> Result<SearchIndex> {
        // Tokenize all content and build inverted index
    }
}
```

This is noted for future consideration but **not included in initial scope**.

### Testing Strategy

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_finds_preview_match() {
        let (storage, _dir) = test_storage();
        storage.save_entry("hello world this is a test").unwrap();

        let mut picker = Picker::new(storage).unwrap();
        picker.search_query = "hello".to_string();
        picker.update_filter();

        assert_eq!(picker.filtered.len(), 1);
        assert_eq!(picker.filtered[0].match_location, MatchLocation::Preview);
    }

    #[test]
    fn test_search_finds_content_match() {
        let (storage, _dir) = test_storage();
        // Create content where searchable text is beyond preview
        let content = format!("{}\nunique_identifier_xyz", "x".repeat(200));
        storage.save_entry(&content).unwrap();

        let mut picker = Picker::new(storage).unwrap();
        picker.search_query = "unique_identifier".to_string();
        picker.update_filter();

        assert_eq!(picker.filtered.len(), 1);
        assert_eq!(picker.filtered[0].match_location, MatchLocation::FullContent);
    }

    #[test]
    fn test_preview_matches_rank_higher() {
        let (storage, _dir) = test_storage();
        // Entry 1: match in preview
        storage.save_entry("findme at the start").unwrap();
        // Entry 2: match only in content
        let content = format!("{}\nfindme buried deep", "x".repeat(200));
        storage.save_entry(&content).unwrap();

        let mut picker = Picker::new(storage).unwrap();
        picker.search_query = "findme".to_string();
        picker.update_filter();

        assert_eq!(picker.filtered.len(), 2);
        // Preview match should be first
        assert_eq!(picker.filtered[0].match_location, MatchLocation::Preview);
    }

    #[test]
    fn test_search_performance_large_history() {
        let (storage, _dir) = test_storage();
        // Create 100 entries
        for i in 0..100 {
            storage.save_entry(&format!("entry number {}", i)).unwrap();
        }

        let mut picker = Picker::new(storage).unwrap();
        let start = std::time::Instant::now();
        picker.search_query = "nonexistent_term".to_string();
        picker.update_filter();
        let elapsed = start.elapsed();

        // Should complete within reasonable time even with full-content search
        assert!(elapsed < std::time::Duration::from_secs(2));
    }
}
```

### Migration Notes

- No storage format changes required
- Backwards compatible; older versions simply won't have this feature
- No user action required

### Success Criteria

- [ ] Searches find content beyond the 100-char preview
- [ ] Preview matches still rank higher than content-only matches
- [ ] Visual indicator distinguishes match location
- [ ] Performance remains acceptable (<500ms for 100 entries)
- [ ] All existing tests pass
- [ ] New tests cover edge cases

---

## Improvement #2: Pinned/Favorites

### Problem Statement

ClipStack enforces a maximum of 100 entries. Older entries are automatically pruned when this limit is reached. This creates a problem for frequently-used text that users want to keep permanently:

- Email signatures
- Code snippets (import statements, boilerplate)
- Template responses
- Credentials/tokens (though these should arguably be in a password manager)

Users currently have no way to prevent important entries from being pruned. They must re-copy these items periodically or use a separate tool.

### Solution Design

Add a "pin" capability that:

1. Marks entries as protected from automatic pruning
2. Shows pinned entries in a prominent location (top of list or separate section)
3. Limits pinned entries to prevent abuse (e.g., max 25 pinned)
4. Persists pin status in the storage index

### User Experience

**Keybindings**:
- `p` in Normal mode: Toggle pin status of selected entry
- Pinned entries show `★` prefix in list
- Status bar shows confirmation: "Pinned" / "Unpinned"

**Visual Mockup**:
```
┌─History (5/105) - 3 pinned────────────────────┐
│> ★ 2d ago [   45B] My email signature...      │ ← Pinned (always at top)
│  ★ 5d ago [  128B] import { useState }...     │ ← Pinned
│  ★ 1w ago [   32B] const API_BASE = ...       │ ← Pinned
│  ──────────────────────────────────────────── │ ← Visual separator
│  10s [  1.2KB] function handleClick()...      │ ← Regular entry
│  2m  [   89B] TODO: fix this bug              │
│  15m [  256B] {"type":"request",...           │
└───────────────────────────────────────────────┘
[NORMAL] p:Pin  j/k:Nav  /:Search  Enter:Paste  d:Delete
```

### Implementation Details

#### File Changes

**`src/storage.rs`** - Add pinned field and modify pruning

```rust
const MAX_ENTRIES: usize = 100;
const MAX_PINNED: usize = 25;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipEntry {
    pub id: String,
    pub timestamp: i64,
    pub size: usize,
    pub preview: String,
    pub hash: String,
    /// Whether this entry is pinned (protected from pruning)
    #[serde(default)]  // Backwards compatible with existing storage
    pub pinned: bool,
}

impl Storage {
    pub fn save_entry(&self, content: &str) -> Result<ClipEntry> {
        // ... existing entry creation logic ...

        let entry = ClipEntry {
            id: id.clone(),
            timestamp,
            size: content.len(),
            preview,
            hash,
            pinned: false,  // New entries are not pinned by default
        };

        // ... save content file ...

        // Update index
        index.entries.insert(0, entry.clone());

        // Modified pruning: only prune non-pinned entries
        self.prune_entries(&mut index)?;

        self.save_index(&index)?;
        Ok(entry)
    }

    /// Prune old entries while respecting pinned status
    fn prune_entries(&self, index: &mut ClipIndex) -> Result<()> {
        // Count non-pinned entries
        let unpinned_count = index.entries.iter().filter(|e| !e.pinned).count();

        // Only prune if we exceed max non-pinned entries
        while unpinned_count > index.max_entries {
            // Find the oldest non-pinned entry
            if let Some(pos) = index.entries.iter().rposition(|e| !e.pinned) {
                let old = index.entries.remove(pos);
                let old_path = self.content_path(&old.id);
                let _ = fs::remove_file(old_path);
            } else {
                break;  // All entries are pinned (shouldn't happen due to MAX_PINNED)
            }
        }

        Ok(())
    }

    /// Toggle pin status of an entry
    pub fn toggle_pin(&self, id: &str) -> Result<bool> {
        let mut index = self.load_index()?;

        // Find the entry
        let entry = index.entries.iter_mut().find(|e| e.id == id);

        match entry {
            Some(entry) => {
                // Check pin limit if pinning
                if !entry.pinned {
                    let pinned_count = index.entries.iter().filter(|e| e.pinned).count();
                    if pinned_count >= MAX_PINNED {
                        anyhow::bail!("Maximum pinned entries ({}) reached", MAX_PINNED);
                    }
                }

                entry.pinned = !entry.pinned;
                let new_status = entry.pinned;
                self.save_index(&index)?;
                Ok(new_status)
            }
            None => anyhow::bail!("Entry not found: {}", id),
        }
    }

    /// Get count of pinned entries
    pub fn pinned_count(&self) -> Result<usize> {
        let index = self.load_index()?;
        Ok(index.entries.iter().filter(|e| e.pinned).count())
    }
}
```

**`src/picker.rs`** - Add pin keybinding and display

```rust
impl Picker {
    fn handle_normal_mode(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> Result<Option<Option<String>>> {
        // ... existing key handling ...

        match key.code {
            // ... existing handlers ...

            // Pin/unpin selected entry
            KeyCode::Char('p') => {
                self.toggle_pin_selected()?;
            }

            // ... rest of handlers ...
        }

        Ok(None)
    }

    fn toggle_pin_selected(&mut self) -> Result<()> {
        if let Some(entry) = self.selected_entry() {
            let id = entry.id.clone();
            match self.storage.toggle_pin(&id) {
                Ok(is_pinned) => {
                    // Update local state
                    if let Some(idx) = self.filtered.get(self.selected.selected().unwrap_or(0)) {
                        self.entries[*idx].pinned = is_pinned;
                    }

                    // Re-sort to move pinned entries to top
                    self.sort_entries();

                    let msg = if is_pinned { "Pinned" } else { "Unpinned" };
                    self.set_status(msg.to_string(), StatusLevel::Success);
                }
                Err(e) => {
                    self.set_status(format!("Error: {}", e), StatusLevel::Warning);
                }
            }
        }
        Ok(())
    }

    /// Sort entries: pinned first, then by timestamp
    fn sort_entries(&mut self) {
        // Store current selection's ID to restore after sort
        let selected_id = self.selected_entry().map(|e| e.id.clone());

        // Sort: pinned entries first, then by timestamp descending
        self.entries.sort_by(|a, b| {
            match (a.pinned, b.pinned) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => b.timestamp.cmp(&a.timestamp),  // Most recent first
            }
        });

        // Update filter indices
        self.update_filter();

        // Restore selection
        if let Some(id) = selected_id {
            if let Some(pos) = self.entries.iter().position(|e| e.id == id) {
                if let Some(filter_pos) = self.filtered.iter().position(|&i| i == pos) {
                    self.selected.select(Some(filter_pos));
                }
            }
        }
    }

    fn render_list(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .map(|&idx| {
                let entry = &self.entries[idx];
                let time = util::format_relative_time(entry.timestamp);
                let size = util::format_size(entry.size);

                let preview: String = entry
                    .preview
                    .chars()
                    .take(30)
                    .collect::<String>()
                    .replace('\n', " ");

                let preview_spans = if !self.search_query.is_empty() {
                    self.highlight_matches(&preview)
                } else {
                    vec![Span::raw(preview)]
                };

                // Pin indicator
                let pin_indicator = if entry.pinned {
                    Span::styled("★ ", Style::default().fg(Color::Yellow))
                } else {
                    Span::raw("  ")
                };

                let mut spans = vec![
                    pin_indicator,
                    Span::styled(
                        format!("{:>3} ", time),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("[{:>5}] ", size),
                        Style::default().fg(Color::Cyan),
                    ),
                ];
                spans.extend(preview_spans);

                ListItem::new(Line::from(spans))
            })
            .collect();

        // Update title to show pinned count
        let pinned_count = self.entries.iter().filter(|e| e.pinned).count();
        let title = if pinned_count > 0 {
            format!(
                "History ({}/{}) - {} pinned{}",
                self.filtered.len(),
                self.entries.len(),
                pinned_count,
                if !self.search_query.is_empty() {
                    format!(" matching '{}'", self.search_query)
                } else {
                    String::new()
                }
            )
        } else {
            format!(
                "History ({}/{}){}",
                self.filtered.len(),
                self.entries.len(),
                if !self.search_query.is_empty() {
                    format!(" matching '{}'", self.search_query)
                } else {
                    String::new()
                }
            )
        };

        // ... rest of render logic with updated title ...
    }

    fn render_status_line(&mut self, frame: &mut Frame, area: Rect) {
        // Update help text to include pin command
        let (text, style) = status_text.unwrap_or_else(|| {
            let mode_indicator = match self.mode {
                Mode::Normal => "[NORMAL]",
                Mode::Search => "[SEARCH]",
            };
            (
                format!(
                    "{} j/k:Nav  p:Pin  /:Search  Enter:Paste  d:Delete  u:Undo  q:Quit",
                    mode_indicator
                ),
                Style::default().fg(Color::DarkGray),
            )
        });
        // ...
    }
}
```

**`src/main.rs`** - Update stats command to show pinned count

```rust
Some(Commands::Stats) => {
    let index = storage.load_index()?;
    let total_size: usize = index.entries.iter().map(|e| e.size).sum();
    let pinned_count = index.entries.iter().filter(|e| e.pinned).count();

    println!("Entries: {}", index.entries.len());
    println!("Pinned: {}", pinned_count);
    println!("Max entries: {}", index.max_entries);
    println!("Total size: {}", util::format_size(total_size));
    // ...
}
```

### Testing Strategy

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pin_entry() {
        let (storage, _dir) = test_storage();
        let entry = storage.save_entry("pin me").unwrap();
        assert!(!entry.pinned);

        let is_pinned = storage.toggle_pin(&entry.id).unwrap();
        assert!(is_pinned);

        // Verify persisted
        let index = storage.load_index().unwrap();
        let loaded = index.entries.iter().find(|e| e.id == entry.id).unwrap();
        assert!(loaded.pinned);
    }

    #[test]
    fn test_unpin_entry() {
        let (storage, _dir) = test_storage();
        let entry = storage.save_entry("unpin me").unwrap();

        storage.toggle_pin(&entry.id).unwrap();  // Pin
        let is_pinned = storage.toggle_pin(&entry.id).unwrap();  // Unpin
        assert!(!is_pinned);
    }

    #[test]
    fn test_pinned_entries_not_pruned() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf()).unwrap();

        // Create a pinned entry
        let pinned = storage.save_entry("keep me").unwrap();
        storage.toggle_pin(&pinned.id).unwrap();

        // Fill up to max + some extra
        for i in 0..105 {
            storage.save_entry(&format!("entry {}", i)).unwrap();
        }

        // Pinned entry should still exist
        let index = storage.load_index().unwrap();
        assert!(index.entries.iter().any(|e| e.id == pinned.id));

        // Should have max_entries unpinned + 1 pinned
        let unpinned_count = index.entries.iter().filter(|e| !e.pinned).count();
        assert!(unpinned_count <= 100);
    }

    #[test]
    fn test_pin_limit() {
        let (storage, _dir) = test_storage();

        // Create and pin MAX_PINNED entries
        for i in 0..MAX_PINNED {
            let entry = storage.save_entry(&format!("pinned {}", i)).unwrap();
            storage.toggle_pin(&entry.id).unwrap();
        }

        // Try to pin one more - should fail
        let extra = storage.save_entry("one too many").unwrap();
        let result = storage.toggle_pin(&extra.id);
        assert!(result.is_err());
    }

    #[test]
    fn test_backwards_compatibility() {
        // Simulate old index format without pinned field
        let dir = TempDir::new().unwrap();
        let index_path = dir.path().join("index.json");

        // Write old format
        fs::write(&index_path, r#"{
            "max_entries": 100,
            "entries": [{
                "id": "123",
                "timestamp": 1000,
                "size": 5,
                "preview": "test",
                "hash": "sha256:abc"
            }]
        }"#).unwrap();

        // Should load without error, pinned defaults to false
        let storage = Storage::new(dir.path().to_path_buf()).unwrap();
        let index = storage.load_index().unwrap();
        assert!(!index.entries[0].pinned);
    }
}
```

### Migration Notes

- The `pinned` field uses `#[serde(default)]`, so existing storage is automatically compatible
- Old entries default to `pinned: false`
- No migration script needed
- Users can start pinning immediately after upgrade

### Success Criteria

- [ ] `p` key toggles pin status with visual feedback
- [ ] Pinned entries show `★` indicator
- [ ] Pinned entries appear at top of list
- [ ] Pinned entries survive pruning
- [ ] Maximum 25 pinned entries enforced
- [ ] `stats` command shows pinned count
- [ ] Backwards compatible with existing storage

---

## Improvement #3: Atomic File Writes

### Problem Statement

The current storage implementation uses non-atomic writes:

```rust
// storage.rs:76
fs::write(&path, data).with_context(|| format!("Failed to write index: {:?}", path))
```

This is dangerous because `fs::write()` is not atomic. If the process is interrupted mid-write (Ctrl+C, system crash, OOM kill, power loss), the file can be left in a corrupted state:

- Truncated JSON (parse error on next load)
- Partially written data (invalid content)
- Empty file (complete data loss)

For a tool whose value is "never lose clipboard history," this is an unacceptable reliability gap.

### Solution Design

Implement the standard atomic write pattern:

1. Write data to a temporary file (`index.json.tmp`)
2. Call `fsync()` to ensure data is flushed to disk
3. Atomically rename temp file to final path
4. On startup, clean up any orphaned temp files

The `rename()` syscall is atomic on POSIX filesystems—it either completes fully or not at all. This guarantees that `index.json` is always in a valid state.

### Technical Background

**Why `fs::write()` is dangerous:**

```
Process: fs::write("index.json", data)
           |
           |-- 1. Open/create file (truncates existing)
           |-- 2. Write chunk 1
           |-- 3. Write chunk 2  <-- CRASH HERE
           |-- 4. Write chunk 3
           |-- 5. Close file

Result: index.json contains only chunks 1-2, is invalid JSON
```

**Why atomic rename is safe:**

```
Process: atomic_write("index.json", data)
           |
           |-- 1. Write to index.json.tmp
           |-- 2. fsync() temp file
           |-- 3. rename(tmp, final)  <-- ATOMIC

If crash during steps 1-2: temp file is corrupt, final untouched
If crash during step 3: rename either happens or doesn't
```

### Implementation Details

#### File Changes

**`src/storage.rs`** - Add atomic write helper

```rust
use std::io::Write;
use std::os::unix::fs::MetadataExt;

impl Storage {
    /// Atomically write data to a file using write-and-rename pattern
    fn atomic_write(&self, path: &Path, data: &[u8]) -> Result<()> {
        let tmp_path = path.with_extension("tmp");

        // Step 1: Write to temporary file
        let mut file = fs::File::create(&tmp_path)
            .with_context(|| format!("Failed to create temp file: {:?}", tmp_path))?;

        file.write_all(data)
            .with_context(|| format!("Failed to write to temp file: {:?}", tmp_path))?;

        // Step 2: Ensure data is on disk
        file.sync_all()
            .with_context(|| format!("Failed to sync temp file: {:?}", tmp_path))?;

        // Explicitly close file before rename
        drop(file);

        // Step 3: Atomic rename
        fs::rename(&tmp_path, path)
            .with_context(|| format!("Failed to rename {:?} to {:?}", tmp_path, path))?;

        // Step 4: Sync parent directory to ensure rename is durable
        // (Required for full durability on some filesystems)
        if let Some(parent) = path.parent() {
            if let Ok(dir) = fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }

        Ok(())
    }

    /// Clean up orphaned temporary files from interrupted operations
    fn cleanup_temp_files(&self) -> Result<()> {
        if let Ok(entries) = fs::read_dir(&self.base_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |e| e == "tmp") {
                    // Log for debugging
                    eprintln!("Cleaning up orphaned temp file: {:?}", path);
                    let _ = fs::remove_file(&path);
                }
            }
        }
        Ok(())
    }

    pub fn new(base_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&base_dir)
            .with_context(|| format!("Failed to create storage dir: {:?}", base_dir))?;

        let storage = Self { base_dir };

        // Clean up any temp files from previous interrupted operations
        storage.cleanup_temp_files()?;

        Ok(storage)
    }

    pub fn save_index(&self, index: &ClipIndex) -> Result<()> {
        let path = self.index_path();
        let data = serde_json::to_string_pretty(index)?;

        // Use atomic write instead of fs::write
        self.atomic_write(&path, data.as_bytes())
    }

    pub fn save_entry(&self, content: &str) -> Result<ClipEntry> {
        // ... existing logic until content write ...

        // Save content to file atomically
        let content_path = self.content_path(&id);
        self.atomic_write(&content_path, content.as_bytes())
            .with_context(|| format!("Failed to write content: {:?}", content_path))?;

        // ... rest of existing logic ...
    }
}
```

#### Recovery Mechanism

In case of corruption (e.g., from bugs in older versions), add a recovery command:

```rust
// In main.rs
enum Commands {
    // ... existing commands ...

    /// Attempt to recover from corrupted storage
    Recover,
}

// Handler
Some(Commands::Recover) => {
    match storage.attempt_recovery() {
        Ok(recovered) => {
            println!("Recovery complete. Recovered {} entries.", recovered);
        }
        Err(e) => {
            eprintln!("Recovery failed: {}", e);
            eprintln!("You may need to manually delete ~/.local/share/clipd/index.json");
        }
    }
}

// In storage.rs
impl Storage {
    /// Attempt to recover from corrupted state
    pub fn attempt_recovery(&self) -> Result<usize> {
        let index_path = self.index_path();

        // Try to load existing index
        let mut recovered_entries = Vec::new();

        if index_path.exists() {
            // Try parsing, handle partial corruption
            match fs::read_to_string(&index_path) {
                Ok(data) => {
                    match serde_json::from_str::<ClipIndex>(&data) {
                        Ok(index) => {
                            recovered_entries = index.entries;
                        }
                        Err(_) => {
                            eprintln!("Index corrupted, scanning content files...");
                        }
                    }
                }
                Err(_) => {
                    eprintln!("Cannot read index, scanning content files...");
                }
            }
        }

        // Scan for content files that might not be in index
        for entry in fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().map_or(false, |e| e == "txt") {
                let id = path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();

                // Skip if already in recovered entries
                if recovered_entries.iter().any(|e| e.id == id) {
                    continue;
                }

                // Try to recover this content file
                if let Ok(content) = fs::read_to_string(&path) {
                    let timestamp: i64 = id.parse().unwrap_or(0);

                    let mut hasher = sha2::Sha256::new();
                    hasher.update(content.as_bytes());
                    let hash = format!("sha256:{:x}", hasher.finalize());

                    let preview: String = content
                        .chars()
                        .take(100)
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .collect();

                    recovered_entries.push(ClipEntry {
                        id,
                        timestamp,
                        size: content.len(),
                        preview,
                        hash,
                        pinned: false,
                    });
                }
            }
        }

        // Sort by timestamp descending
        recovered_entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        // Deduplicate by hash
        let mut seen_hashes = std::collections::HashSet::new();
        recovered_entries.retain(|e| seen_hashes.insert(e.hash.clone()));

        let count = recovered_entries.len();

        // Save recovered index
        let index = ClipIndex {
            max_entries: MAX_ENTRIES,
            entries: recovered_entries,
        };
        self.save_index(&index)?;

        Ok(count)
    }
}
```

### Testing Strategy

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn test_atomic_write_creates_file() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf()).unwrap();
        let test_path = dir.path().join("test.json");

        storage.atomic_write(&test_path, b"test data").unwrap();

        assert!(test_path.exists());
        assert_eq!(fs::read_to_string(&test_path).unwrap(), "test data");
    }

    #[test]
    fn test_atomic_write_no_temp_file_remains() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf()).unwrap();
        let test_path = dir.path().join("test.json");

        storage.atomic_write(&test_path, b"test data").unwrap();

        // No .tmp file should exist
        let tmp_path = test_path.with_extension("tmp");
        assert!(!tmp_path.exists());
    }

    #[test]
    fn test_cleanup_orphaned_temp_files() {
        let dir = TempDir::new().unwrap();

        // Create orphaned temp files
        fs::write(dir.path().join("index.tmp"), "orphaned").unwrap();
        fs::write(dir.path().join("123.tmp"), "orphaned").unwrap();

        // Storage::new should clean them up
        let _storage = Storage::new(dir.path().to_path_buf()).unwrap();

        assert!(!dir.path().join("index.tmp").exists());
        assert!(!dir.path().join("123.tmp").exists());
    }

    #[test]
    fn test_atomic_write_overwrites_existing() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf()).unwrap();
        let test_path = dir.path().join("test.json");

        // Write initial content
        storage.atomic_write(&test_path, b"initial").unwrap();
        assert_eq!(fs::read_to_string(&test_path).unwrap(), "initial");

        // Overwrite
        storage.atomic_write(&test_path, b"updated").unwrap();
        assert_eq!(fs::read_to_string(&test_path).unwrap(), "updated");
    }

    #[test]
    fn test_recovery_from_missing_index() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf()).unwrap();

        // Create some content files without an index
        fs::write(dir.path().join("1000.txt"), "content one").unwrap();
        fs::write(dir.path().join("2000.txt"), "content two").unwrap();
        fs::remove_file(dir.path().join("index.json")).ok();

        // Recover
        let count = storage.attempt_recovery().unwrap();
        assert_eq!(count, 2);

        // Index should now exist and be valid
        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), 2);
    }

    #[test]
    fn test_concurrent_writes_safe() {
        use std::thread;

        let dir = TempDir::new().unwrap();
        let storage = Arc::new(Storage::new(dir.path().to_path_buf()).unwrap());

        let mut handles = vec![];
        for i in 0..10 {
            let storage = Arc::clone(&storage);
            handles.push(thread::spawn(move || {
                storage.save_entry(&format!("content {}", i)).unwrap();
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // All entries should be saved
        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), 10);
    }
}
```

### Platform Considerations

- **Linux/macOS**: `rename()` is atomic on all major filesystems (ext4, btrfs, APFS, HFS+)
- **Windows**: `rename()` is atomic on NTFS, but requires `MoveFileEx` with `MOVEFILE_REPLACE_EXISTING`
- **Network filesystems**: NFS v3 has non-atomic rename; v4 is atomic. For maximum safety, avoid storing clipboard data on network mounts.

The Rust `fs::rename()` handles platform differences, so no special code is needed.

### Success Criteria

- [ ] All writes use atomic pattern
- [ ] Orphaned temp files cleaned up on startup
- [ ] Recovery command can rebuild index from content files
- [ ] No data loss possible from interrupted writes
- [ ] Concurrent access doesn't corrupt storage

---

## Improvement #4: Preview Scrolling

### Problem Statement

The preview pane shows a fixed view of clipboard content. For long entries, users see only the first ~30 lines with a "[+N lines]" indicator. There's no way to view the rest of the content without:

1. Selecting the entry
2. Pasting somewhere
3. Reading the pasted content
4. Deciding if it's the right entry
5. Undoing the paste if wrong
6. Trying another entry

This workflow is tedious and disruptive. Users should be able to scroll through content before committing to a selection.

### Solution Design

Add scrolling capability to the preview pane:

- Track scroll offset state
- Keybindings to scroll preview content
- Visual indicator of scroll position
- Reset scroll when selection changes

### User Experience

**Keybindings** (in Normal mode when preview is focused):
- `Ctrl+J` / `Ctrl+Down`: Scroll preview down 5 lines
- `Ctrl+K` / `Ctrl+Up`: Scroll preview up 5 lines
- `Ctrl+D`: Scroll preview down half page
- `Ctrl+U`: Scroll preview up half page

**Visual Mockup**:
```
┌─Preview - 2.3KB - 5m ago [lines 45-75/150]────┐
│    return {                                    │
│      user: data.user,                          │
│      token: generateToken(data.user.id),       │
│      permissions: await getPermissions(        │
│        data.user.role                          │
│      ),                                        │
│    };                                          │
│  } catch (error) {                             │
│    logger.error('Auth failed', { error });     │
│    throw new AuthenticationError(error);       │
│  }                                             │
│}                                              ▼│ ← Scroll indicator
└───────────────────────────────────────────────┘
```

### Implementation Details

#### File Changes

**`src/picker.rs`** - Add scroll state and handlers

```rust
pub struct Picker {
    // ... existing fields ...

    /// Scroll offset for preview pane (in lines)
    preview_scroll: usize,
}

impl Picker {
    pub fn new(storage: Storage) -> Result<Self> {
        // ... existing initialization ...

        let mut picker = Self {
            storage,
            entries: index.entries,
            filtered: Vec::new(),
            selected: ListState::default(),
            scroll_state: ScrollbarState::default(),
            search_query: String::new(),
            preview_content: None,
            preview_id: None,
            preview_scroll: 0,  // New field
            matcher: SkimMatcherV2::default(),
            mode: Mode::Normal,
            status_message: None,
            last_deleted: None,
            pending_g: false,
        };

        // ... rest of initialization ...
    }

    fn load_preview(&mut self) {
        let entry_id = self.selected_entry().map(|e| e.id.clone());

        match entry_id {
            Some(id) if self.preview_id.as_ref() != Some(&id) => {
                match self.storage.load_content(&id) {
                    Ok(content) => {
                        self.preview_content = Some(content);
                        self.preview_id = Some(id);
                        self.preview_scroll = 0;  // Reset scroll on new selection
                    }
                    Err(_) => {
                        self.preview_content = None;
                        self.preview_id = None;
                        self.preview_scroll = 0;
                    }
                }
            }
            None => {
                self.preview_content = None;
                self.preview_id = None;
                self.preview_scroll = 0;
            }
            _ => {}
        }
    }

    /// Scroll preview by delta lines (positive = down, negative = up)
    fn scroll_preview(&mut self, delta: i32) {
        if let Some(content) = &self.preview_content {
            let total_lines = content.lines().count();
            let new_scroll = if delta > 0 {
                self.preview_scroll.saturating_add(delta as usize)
            } else {
                self.preview_scroll.saturating_sub((-delta) as usize)
            };

            // Clamp to valid range (allow scrolling until last line is visible)
            self.preview_scroll = new_scroll.min(total_lines.saturating_sub(1));
        }
    }

    fn render_preview(&self, frame: &mut Frame, area: Rect) {
        let (content, metadata) = if let Some(entry) = self.selected_entry() {
            let content = self.preview_content.as_deref().unwrap_or("(loading...)");
            let time = util::format_relative_time(entry.timestamp);
            let size = util::format_size(entry.size);
            (content, format!("{} - {}", size, time))
        } else {
            ("(no selection)", "Preview".to_string())
        };

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();
        let visible_height = (area.height.saturating_sub(2)) as usize;

        // Apply scroll offset
        let visible_lines: Vec<&str> = lines
            .iter()
            .skip(self.preview_scroll)
            .take(visible_height)
            .copied()
            .collect();

        let preview_text = visible_lines.join("\n");

        // Build title with scroll position
        let title = if total_lines > visible_height {
            let start_line = self.preview_scroll + 1;
            let end_line = (self.preview_scroll + visible_height).min(total_lines);
            format!(
                "Preview - {} [lines {}-{}/{}]",
                metadata, start_line, end_line, total_lines
            )
        } else {
            format!("Preview - {}", metadata)
        };

        // Determine if we can scroll (for visual hint)
        let can_scroll_down = self.preview_scroll + visible_height < total_lines;
        let can_scroll_up = self.preview_scroll > 0;

        let border_style = if can_scroll_down || can_scroll_up {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let preview = Paragraph::new(preview_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(border_style),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(preview, area);

        // Render scroll indicators in corners
        if can_scroll_up {
            let up_indicator = Paragraph::new("▲")
                .style(Style::default().fg(Color::Yellow));
            let up_area = Rect {
                x: area.x + area.width - 2,
                y: area.y,
                width: 1,
                height: 1,
            };
            frame.render_widget(up_indicator, up_area);
        }

        if can_scroll_down {
            let down_indicator = Paragraph::new("▼")
                .style(Style::default().fg(Color::Yellow));
            let down_area = Rect {
                x: area.x + area.width - 2,
                y: area.y + area.height - 1,
                width: 1,
                height: 1,
            };
            frame.render_widget(down_indicator, down_area);
        }
    }

    fn handle_normal_mode(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> Result<Option<Option<String>>> {
        // ... existing handlers ...

        match key.code {
            // ... existing key handlers ...

            // Preview scrolling
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_preview(5);
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_preview(-5);
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Half-page down (for preview, not list)
                // Note: This overrides the existing Ctrl+D for list navigation
                // We'll use Ctrl+D for preview scroll, PageDown for list
                self.scroll_preview(15);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Half-page up (for preview, not list)
                self.scroll_preview(-15);
            }

            // ... rest of handlers ...
        }

        Ok(None)
    }

    fn render_status_line(&mut self, frame: &mut Frame, area: Rect) {
        // Update help to show preview scroll keys
        let (text, style) = status_text.unwrap_or_else(|| {
            let mode_indicator = match self.mode {
                Mode::Normal => "[NORMAL]",
                Mode::Search => "[SEARCH]",
            };
            (
                format!(
                    "{} j/k:Nav  Ctrl+j/k:Scroll  Enter:Paste  d:Del  p:Pin  q:Quit",
                    mode_indicator
                ),
                Style::default().fg(Color::DarkGray),
            )
        });
        // ...
    }
}
```

### Keybinding Conflict Resolution

The original implementation uses `Ctrl+D` / `Ctrl+U` for list navigation (page up/down). With preview scrolling, we have a conflict. Options:

1. **Option A**: Use `Ctrl+D/U` for preview scroll, `PageDown/PageUp` for list
2. **Option B**: Use `Ctrl+J/K` for preview scroll, keep `Ctrl+D/U` for list
3. **Option C**: Add a "focus" concept (Tab to switch focus between list and preview)

**Recommendation**: Option B is safest—it's additive and doesn't change existing behavior. `Ctrl+J/K` mirrors vim's navigation and is intuitive for scrolling.

### Testing Strategy

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_preview_scroll_down() {
        let (storage, _dir) = test_storage();
        let content = (0..100).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        storage.save_entry(&content).unwrap();

        let mut picker = Picker::new(storage).unwrap();
        picker.load_preview();

        assert_eq!(picker.preview_scroll, 0);

        picker.scroll_preview(10);
        assert_eq!(picker.preview_scroll, 10);
    }

    #[test]
    fn test_preview_scroll_clamps_to_content() {
        let (storage, _dir) = test_storage();
        let content = "line1\nline2\nline3";  // Only 3 lines
        storage.save_entry(content).unwrap();

        let mut picker = Picker::new(storage).unwrap();
        picker.load_preview();

        picker.scroll_preview(100);  // Try to scroll way past end
        assert!(picker.preview_scroll <= 2);  // Should clamp
    }

    #[test]
    fn test_preview_scroll_negative_clamps_to_zero() {
        let (storage, _dir) = test_storage();
        storage.save_entry("content").unwrap();

        let mut picker = Picker::new(storage).unwrap();
        picker.load_preview();

        picker.scroll_preview(-100);  // Try to scroll before start
        assert_eq!(picker.preview_scroll, 0);
    }

    #[test]
    fn test_preview_scroll_resets_on_selection_change() {
        let (storage, _dir) = test_storage();
        let content = (0..100).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        storage.save_entry(&content).unwrap();
        storage.save_entry("another entry").unwrap();

        let mut picker = Picker::new(storage).unwrap();
        picker.load_preview();
        picker.scroll_preview(50);
        assert_eq!(picker.preview_scroll, 50);

        // Change selection
        picker.move_selection(1);
        assert_eq!(picker.preview_scroll, 0);  // Should reset
    }
}
```

### Success Criteria

- [ ] `Ctrl+J/K` scrolls preview up/down by 5 lines
- [ ] Scroll position indicator shows current line range
- [ ] Scroll clamps to valid range (no scrolling past content)
- [ ] Scroll resets when selection changes
- [ ] Visual indicators (▲/▼) show when scrolling is possible
- [ ] Existing list navigation keybindings unchanged

---

## Improvement #5: Configurable Max Entries

### Problem Statement

The maximum number of stored entries is hardcoded:

```rust
// storage.rs:8
const MAX_ENTRIES: usize = 100;
```

This one-size-fits-all default doesn't serve all users:

- **Power users** may want 500+ entries for deep history across projects
- **Privacy-conscious users** may want only 10-20 entries
- **Resource-constrained systems** may need limits to control storage size

Currently, adjusting this requires modifying source code and recompiling.

### Solution Design

Make max entries configurable via:

1. **CLI flag**: `--max-entries N` (highest priority)
2. **Environment variable**: `CLIPSTACK_MAX_ENTRIES` (medium priority)
3. **Stored preference**: Saved in index.json (lowest priority)

The CLI flag allows per-command overrides while the stored preference provides a persistent default.

### Implementation Details

#### File Changes

**`src/main.rs`** - Add CLI flag

```rust
#[derive(Parser)]
#[command(name = "clipstack")]
#[command(about = "Fast clipboard manager with lazy-loading history")]
#[command(version)]
struct Cli {
    /// Custom storage directory
    #[arg(long, global = true)]
    storage_dir: Option<PathBuf>,

    /// Maximum entries to store (default: 100, max: 10000)
    #[arg(long, global = true, value_parser = clap::value_parser!(u32).range(1..=10000))]
    max_entries: Option<u32>,

    #[command(subcommand)]
    command: Option<Commands>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Determine max_entries from CLI > env > default
    let max_entries = cli.max_entries
        .map(|n| n as usize)
        .or_else(|| std::env::var("CLIPSTACK_MAX_ENTRIES").ok().and_then(|s| s.parse().ok()))
        .unwrap_or(100);

    // Validate range
    let max_entries = max_entries.clamp(1, 10000);

    let storage_dir = cli.storage_dir.unwrap_or_else(storage::Storage::default_dir);
    let storage = storage::Storage::new(storage_dir, max_entries)?;

    // ... rest of main ...
}
```

**`src/storage.rs`** - Accept max_entries parameter

```rust
const DEFAULT_MAX_ENTRIES: usize = 100;
const ABSOLUTE_MAX_ENTRIES: usize = 10000;  // Safety limit

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipIndex {
    pub max_entries: usize,
    pub entries: Vec<ClipEntry>,
}

impl Default for ClipIndex {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_MAX_ENTRIES,
            entries: Vec::new(),
        }
    }
}

pub struct Storage {
    base_dir: PathBuf,
    max_entries: usize,
}

impl Storage {
    pub fn new(base_dir: PathBuf, max_entries: usize) -> Result<Self> {
        fs::create_dir_all(&base_dir)
            .with_context(|| format!("Failed to create storage dir: {:?}", base_dir))?;

        // Clamp to safety limits
        let max_entries = max_entries.clamp(1, ABSOLUTE_MAX_ENTRIES);

        let storage = Self { base_dir, max_entries };
        storage.cleanup_temp_files()?;

        // Update stored max_entries if different
        storage.update_max_entries_if_needed()?;

        Ok(storage)
    }

    /// Convenience constructor using default max_entries
    pub fn with_defaults(base_dir: PathBuf) -> Result<Self> {
        Self::new(base_dir, DEFAULT_MAX_ENTRIES)
    }

    fn update_max_entries_if_needed(&self) -> Result<()> {
        let mut index = self.load_index()?;

        if index.max_entries != self.max_entries {
            index.max_entries = self.max_entries;

            // If reducing max_entries, prune immediately
            if index.entries.len() > self.max_entries {
                self.prune_entries(&mut index)?;
            }

            self.save_index(&index)?;
        }

        Ok(())
    }

    pub fn save_entry(&self, content: &str) -> Result<ClipEntry> {
        // ... existing logic ...

        // Update index using instance max_entries
        index.entries.insert(0, entry.clone());

        // Prune using instance max_entries, not stored value
        while index.entries.iter().filter(|e| !e.pinned).count() > self.max_entries {
            if let Some(pos) = index.entries.iter().rposition(|e| !e.pinned) {
                let old = index.entries.remove(pos);
                let old_path = self.content_path(&old.id);
                let _ = fs::remove_file(old_path);
            } else {
                break;
            }
        }

        self.save_index(&index)?;
        Ok(entry)
    }

    pub fn max_entries(&self) -> usize {
        self.max_entries
    }
}
```

**`src/main.rs`** - Update stats to show configured max

```rust
Some(Commands::Stats) => {
    let index = storage.load_index()?;
    let total_size: usize = index.entries.iter().map(|e| e.size).sum();
    let pinned_count = index.entries.iter().filter(|e| e.pinned).count();

    println!("Entries: {}/{}", index.entries.len(), storage.max_entries());
    if pinned_count > 0 {
        println!("Pinned: {} (not counted against limit)", pinned_count);
    }
    println!("Total size: {}", util::format_size(total_size));

    if let Some(oldest) = index.entries.last() {
        println!("Oldest: {}", util::format_relative_time(oldest.timestamp));
    }
    if let Some(newest) = index.entries.first() {
        println!("Newest: {}", util::format_relative_time(newest.timestamp));
    }
}
```

**Update `clipstack status`** to show configuration:

```rust
Some(Commands::Status) => {
    // ... existing status checks ...

    println!("\nConfiguration:");
    println!("  Max entries: {}", storage.max_entries());
    if let Ok(env_val) = std::env::var("CLIPSTACK_MAX_ENTRIES") {
        println!("  (from CLIPSTACK_MAX_ENTRIES={})", env_val);
    }
}
```

### Shell Completion Updates

Update completions to suggest common values:

```rust
Some(Commands::Completions { shell }) => {
    // Custom completions for --max-entries could suggest: 50, 100, 250, 500, 1000
    generate_completions(shell);
}
```

### Documentation

Update `--help` output:

```
OPTIONS:
    --max-entries <N>    Maximum entries to store [default: 100] [max: 10000]
                         Can also be set via CLIPSTACK_MAX_ENTRIES env var
    --storage-dir <DIR>  Custom storage directory
```

### Testing Strategy

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_custom_max_entries() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 5).unwrap();

        // Fill beyond limit
        for i in 0..10 {
            storage.save_entry(&format!("entry {}", i)).unwrap();
        }

        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), 5);
        assert_eq!(index.max_entries, 5);
    }

    #[test]
    fn test_max_entries_clamps_to_range() {
        let dir = TempDir::new().unwrap();

        // Try to set 0 (below minimum)
        let storage = Storage::new(dir.path().to_path_buf(), 0).unwrap();
        assert_eq!(storage.max_entries(), 1);

        // Try to set huge value
        let storage2 = Storage::new(dir.path().to_path_buf(), 999999).unwrap();
        assert_eq!(storage2.max_entries(), 10000);
    }

    #[test]
    fn test_reducing_max_entries_prunes_immediately() {
        let dir = TempDir::new().unwrap();

        // Create storage with high limit
        let storage = Storage::new(dir.path().to_path_buf(), 100).unwrap();
        for i in 0..50 {
            storage.save_entry(&format!("entry {}", i)).unwrap();
        }

        // Recreate with lower limit
        let storage = Storage::new(dir.path().to_path_buf(), 10).unwrap();
        let index = storage.load_index().unwrap();

        assert_eq!(index.entries.len(), 10);
    }

    #[test]
    fn test_pinned_entries_respect_separate_limit() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 5).unwrap();

        // Pin some entries
        for i in 0..3 {
            let entry = storage.save_entry(&format!("pinned {}", i)).unwrap();
            storage.toggle_pin(&entry.id).unwrap();
        }

        // Fill with unpinned
        for i in 0..10 {
            storage.save_entry(&format!("unpinned {}", i)).unwrap();
        }

        let index = storage.load_index().unwrap();
        let pinned_count = index.entries.iter().filter(|e| e.pinned).count();
        let unpinned_count = index.entries.iter().filter(|e| !e.pinned).count();

        assert_eq!(pinned_count, 3);  // Pinned entries preserved
        assert_eq!(unpinned_count, 5);  // Unpinned capped at max_entries
    }
}
```

### Success Criteria

- [ ] `--max-entries N` CLI flag works
- [ ] `CLIPSTACK_MAX_ENTRIES` environment variable works
- [ ] Values clamped to 1-10000 range
- [ ] Reducing max_entries prunes old entries immediately
- [ ] `stats` command shows current limit
- [ ] Pinned entries don't count against limit
- [ ] Existing storage remains compatible

---

## Implementation Roadmap

### Phase 1: Foundation (Improvements #3 and #5)

**Goal**: Establish reliability and configurability foundations.

1. **Atomic File Writes** (#3) - Critical reliability fix
   - Implement `atomic_write()` helper
   - Add temp file cleanup
   - Add recovery command
   - Estimated: 2-3 hours

2. **Configurable Max Entries** (#5) - Simple, unlocks testing
   - Add CLI flag and env var
   - Update Storage constructor
   - Update stats/status commands
   - Estimated: 1-2 hours

### Phase 2: Core Features (Improvements #1 and #2)

**Goal**: Add the most user-impactful features.

3. **Pinned/Favorites** (#2) - High-value, low-effort
   - Add `pinned` field
   - Add toggle keybinding
   - Update pruning logic
   - Update display
   - Estimated: 2-3 hours

4. **Full Content Search** (#1) - Highest impact
   - Implement two-phase search
   - Add match location tracking
   - Add visual indicator
   - Estimated: 3-4 hours

### Phase 3: Polish (Improvement #4)

**Goal**: Complete the UX refinements.

5. **Preview Scrolling** (#4) - UX completion
   - Add scroll state
   - Add keybindings
   - Add scroll indicators
   - Estimated: 2-3 hours

### Total Estimated Effort

| Phase | Improvements | Effort |
|-------|--------------|--------|
| 1 | #3, #5 | 3-5 hours |
| 2 | #2, #1 | 5-7 hours |
| 3 | #4 | 2-3 hours |
| **Total** | All 5 | **10-15 hours** |

---

## Testing Requirements

### Unit Tests

Each improvement must include:

- Happy path tests
- Edge case tests
- Error handling tests
- Backwards compatibility tests

### Integration Tests

- Full workflow tests (daemon + picker + copy/paste)
- Storage migration tests
- Concurrent access tests

### Manual Testing Checklist

Before each release:

- [ ] Fresh install works
- [ ] Upgrade from previous version works
- [ ] All keybindings work as documented
- [ ] Performance acceptable with 100+ entries
- [ ] Unicode content handled correctly
- [ ] Daemon starts/stops cleanly

---

## Appendix: Rejected Alternatives

### Why Not Image/Binary Support?

While valuable, image support would require:
- Binary storage format
- Image preview rendering (or placeholder)
- MIME type detection and handling
- Significantly larger storage footprint

This is better suited for a v2.0 release after the core text functionality is polished.

### Why Not Config File System?

A TOML/YAML config file would be nice but:
- Adds dependency (toml or yaml crate)
- Requires config file discovery logic
- Opens scope for many more options
- CLI flags and env vars cover the immediate need

Config file can be added later when more options exist.

### Why Not D-Bus Interface?

D-Bus would enable external integrations but:
- Linux-only
- Adds significant complexity
- Most users don't need programmatic access
- JSON storage is already accessible for scripts

### Why Not Auto-Classification?

While nice for UX, auto-classification:
- Adds heuristic complexity
- May misclassify content
- Doesn't solve a core problem
- Can be added later as polish

---

## Conclusion

These five improvements target the highest-value, most pragmatic enhancements for ClipStack:

1. **Full Content Search** - Fixes a fundamental usability gap
2. **Pinned/Favorites** - Adds a highly-requested feature
3. **Atomic Writes** - Eliminates a reliability risk
4. **Preview Scrolling** - Completes the preview UX
5. **Configurable Max Entries** - Enables user customization

Together, they transform ClipStack from a functional clipboard manager into a robust, user-friendly tool that competes with established alternatives.
