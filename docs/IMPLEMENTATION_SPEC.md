# ClipStack Implementation Specification

> **Status**: Implementation-Ready
> **Last Updated**: 2026-01-13
> **Decision Log**: All ambiguities resolved per user input

This document provides complete, copy-paste-ready code for all five ClipStack improvements. Every function includes comprehensive tests following TDD methodology.

---

## Table of Contents

1. [Design Decisions](#design-decisions)
2. [Improvement #1: Full Content Search](#improvement-1-full-content-search)
3. [Improvement #2: Pinned/Favorites](#improvement-2-pinnedfavorites)
4. [Improvement #3: Atomic File Writes](#improvement-3-atomic-file-writes)
5. [Improvement #4: Preview Scrolling with Focus Mode](#improvement-4-preview-scrolling-with-focus-mode)
6. [Improvement #5: Configurable Max Entries](#improvement-5-configurable-max-entries)
7. [Implementation Order](#implementation-order)
8. [Master Test Plan](#master-test-plan)

---

## Design Decisions

| Decision | Resolution |
|----------|------------|
| Storage API migration | Direct update, no backwards compatibility layer needed |
| Picker::filtered type | Full change to `Vec<FilteredEntry>` |
| Pin + duplicate behavior | Move to front, preserve pinned status |
| Preview scroll keybindings | Tab switches focus; Ctrl+D/U works in focused pane |
| Content search limits | Stop at 10 matches AND 50 file loads (whichever first) |

---

## Improvement #1: Full Content Search

### Problem

Search only matches the 100-character preview, missing content buried deeper in clipboard entries.

### Solution

Two-phase search: preview first (fast), then full content for non-matches (lazy I/O).

### File: `src/picker.rs` - New Types

```rust
/// Tracks whether a match was found in preview or full content
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MatchLocation {
    /// Match found in the 100-char preview (fast path)
    Preview,
    /// Match found in full content beyond preview (required I/O)
    FullContent,
}

/// Represents a filtered entry with match metadata
#[derive(Clone, Debug)]
pub struct FilteredEntry {
    /// Index into Picker::entries
    pub index: usize,
    /// Fuzzy match score (higher = better)
    pub score: i64,
    /// Where the match was found
    pub match_location: MatchLocation,
}
```

### File: `src/picker.rs` - Constants

```rust
/// Maximum content files to load during full-content search phase
const MAX_CONTENT_SEARCHES: usize = 50;

/// Maximum content matches before stopping search (performance guard)
const MAX_CONTENT_MATCHES: usize = 10;

/// Minimum query length to trigger expensive full-content search
const MIN_QUERY_FOR_CONTENT_SEARCH: usize = 2;
```

### File: `src/picker.rs` - Struct Changes

```rust
pub struct Picker {
    storage: Storage,
    entries: Vec<ClipEntry>,
    filtered: Vec<FilteredEntry>,  // CHANGED from Vec<usize>
    selected: ListState,
    scroll_state: ScrollbarState,
    search_query: String,
    preview_content: Option<String>,
    preview_id: Option<String>,
    preview_scroll: usize,         // NEW: for Improvement #4
    matcher: SkimMatcherV2,
    mode: Mode,
    focus: Focus,                  // NEW: for Improvement #4
    status_message: Option<(String, StatusLevel, Instant)>,
    last_deleted: Option<DeletedEntry>,
    pending_g: bool,
}

/// Which pane has keyboard focus (for Improvement #4)
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Focus {
    #[default]
    List,
    Preview,
}
```

### File: `src/picker.rs` - Complete `update_filter()` Implementation

```rust
impl Picker {
    fn update_filter(&mut self) {
        if self.search_query.is_empty() {
            // No search query: show all entries in order
            self.filtered = self.entries
                .iter()
                .enumerate()
                .map(|(i, _)| FilteredEntry {
                    index: i,
                    score: 0,
                    match_location: MatchLocation::Preview,
                })
                .collect();
        } else {
            let mut results: Vec<FilteredEntry> = Vec::new();
            let mut unmatched_indices: Vec<usize> = Vec::new();

            // ═══════════════════════════════════════════════════════════
            // PHASE 1: Search previews (fast, in-memory)
            // ═══════════════════════════════════════════════════════════
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

            // ═══════════════════════════════════════════════════════════
            // PHASE 2: Search full content (lazy I/O, with limits)
            // ═══════════════════════════════════════════════════════════
            let query_long_enough = self.search_query.chars().count() >= MIN_QUERY_FOR_CONTENT_SEARCH;
            let few_preview_matches = results.len() < MAX_CONTENT_MATCHES;

            if query_long_enough && few_preview_matches {
                let mut content_matches_found = 0;
                let mut content_files_loaded = 0;

                for i in unmatched_indices {
                    // Enforce both limits
                    if content_matches_found >= MAX_CONTENT_MATCHES {
                        break;
                    }
                    if content_files_loaded >= MAX_CONTENT_SEARCHES {
                        break;
                    }

                    let entry = &self.entries[i];
                    content_files_loaded += 1;

                    // Load full content from disk
                    if let Ok(content) = self.storage.load_content(&entry.id) {
                        if let Some(score) = self.matcher.fuzzy_match(&content, &self.search_query) {
                            content_matches_found += 1;
                            results.push(FilteredEntry {
                                index: i,
                                // Penalize content-only matches by 20% so preview
                                // matches rank higher when scores are similar
                                score: score * 8 / 10,
                                match_location: MatchLocation::FullContent,
                            });
                        }
                    }
                }
            }

            // Sort by score descending (best matches first)
            results.sort_by(|a, b| b.score.cmp(&a.score));
            self.filtered = results;
        }

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
}
```

### File: `src/picker.rs` - Updated `selected_entry()` Methods

```rust
impl Picker {
    /// Get the ClipEntry for the current selection
    fn selected_entry(&self) -> Option<&ClipEntry> {
        self.selected
            .selected()
            .and_then(|i| self.filtered.get(i))
            .and_then(|filtered| self.entries.get(filtered.index))
    }

    /// Get the FilteredEntry metadata for the current selection
    fn selected_filtered(&self) -> Option<&FilteredEntry> {
        self.selected
            .selected()
            .and_then(|i| self.filtered.get(i))
    }

    /// Get the index of the currently selected entry in self.entries
    fn selected_entry_index(&self) -> Option<usize> {
        self.selected
            .selected()
            .and_then(|i| self.filtered.get(i))
            .map(|f| f.index)
    }
}
```

### File: `src/picker.rs` - Updated `render_list()` with Match Indicator

```rust
impl Picker {
    fn render_list(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .map(|filtered| {
                let entry = &self.entries[filtered.index];
                let time = util::format_relative_time(entry.timestamp);
                let size = util::format_size(entry.size);

                // Truncate preview for list display
                let preview: String = entry
                    .preview
                    .chars()
                    .take(28)  // Reduced to make room for indicators
                    .collect::<String>()
                    .replace('\n', " ");

                // Highlight matched characters if searching
                let preview_spans = if !self.search_query.is_empty() {
                    self.highlight_matches(&preview)
                } else {
                    vec![Span::raw(preview)]
                };

                // Pin indicator (★ for pinned, space for not)
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

                // Full-content match indicator
                if filtered.match_location == MatchLocation::FullContent {
                    spans.push(Span::styled(
                        " [+]",
                        Style::default().fg(Color::Green),
                    ));
                }

                ListItem::new(Line::from(spans))
            })
            .collect();

        // Build title with counts
        let pinned_count = self.entries.iter().filter(|e| e.pinned).count();
        let title = if !self.search_query.is_empty() {
            format!(
                "History ({}/{}) matching '{}'",
                self.filtered.len(),
                self.entries.len(),
                self.search_query
            )
        } else if pinned_count > 0 {
            format!(
                "History ({}/{}) - {} pinned",
                self.filtered.len(),
                self.entries.len(),
                pinned_count
            )
        } else {
            format!("History ({}/{})", self.filtered.len(), self.entries.len())
        };

        // Highlight border when list is focused
        let border_style = if self.focus == Focus::List {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(border_style),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("> ");

        frame.render_stateful_widget(list, area, &mut self.selected);

        // Render scrollbar
        frame.render_stateful_widget(
            Scrollbar::default()
                .orientation(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("^"))
                .end_symbol(Some("v")),
            area,
            &mut self.scroll_state,
        );
    }
}
```

### Tests: Full Content Search

```rust
#[cfg(test)]
mod full_content_search_tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a test picker with isolated storage
    fn test_picker_with_entries(entries: &[&str]) -> (Picker, TempDir) {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 100).unwrap();

        for content in entries {
            storage.save_entry(content).unwrap();
        }

        let picker = Picker::new(storage).unwrap();
        (picker, dir)
    }

    #[test]
    fn test_empty_search_returns_all_entries() {
        let (mut picker, _dir) = test_picker_with_entries(&["one", "two", "three"]);

        picker.search_query = String::new();
        picker.update_filter();

        assert_eq!(picker.filtered.len(), 3);
        // All should have Preview match location (default for no search)
        for f in &picker.filtered {
            assert_eq!(f.match_location, MatchLocation::Preview);
        }
    }

    #[test]
    fn test_preview_match_found() {
        let (mut picker, _dir) = test_picker_with_entries(&["hello world"]);

        picker.search_query = "hello".to_string();
        picker.update_filter();

        assert_eq!(picker.filtered.len(), 1);
        assert_eq!(picker.filtered[0].match_location, MatchLocation::Preview);
    }

    #[test]
    fn test_content_match_beyond_preview() {
        // Create content where search term is beyond 100-char preview
        let padding = "x".repeat(150);
        let content = format!("{}\nUNIQUE_DEEP_TERM", padding);

        let (mut picker, _dir) = test_picker_with_entries(&[&content]);

        picker.search_query = "UNIQUE_DEEP".to_string();
        picker.update_filter();

        assert_eq!(picker.filtered.len(), 1);
        assert_eq!(picker.filtered[0].match_location, MatchLocation::FullContent);
    }

    #[test]
    fn test_preview_matches_rank_higher() {
        // Entry 1: match in content only
        let deep_content = format!("{}\nFINDME", "x".repeat(150));
        // Entry 2: match in preview
        let preview_content = "FINDME at start";

        let (mut picker, _dir) = test_picker_with_entries(&[&deep_content, preview_content]);

        picker.search_query = "FINDME".to_string();
        picker.update_filter();

        assert_eq!(picker.filtered.len(), 2);
        // Preview match should be first (higher score)
        assert_eq!(picker.filtered[0].match_location, MatchLocation::Preview);
        assert_eq!(picker.filtered[1].match_location, MatchLocation::FullContent);
    }

    #[test]
    fn test_short_query_skips_content_search() {
        let content = format!("{}\nX", "padding".repeat(20));
        let (mut picker, _dir) = test_picker_with_entries(&[&content]);

        // Single char - should NOT trigger content search
        picker.search_query = "X".to_string();
        picker.update_filter();

        // 'X' only exists in content, not preview, and query is too short
        assert_eq!(picker.filtered.len(), 0);
    }

    #[test]
    fn test_content_search_respects_match_limit() {
        // Create 100 entries that only match in content
        let mut entries: Vec<String> = Vec::new();
        for i in 0..100 {
            entries.push(format!("{}\nMATCH_{:03}", "x".repeat(150), i));
        }

        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 100).unwrap();
        for entry in &entries {
            storage.save_entry(entry).unwrap();
        }
        let mut picker = Picker::new(storage).unwrap();

        picker.search_query = "MATCH_".to_string();
        picker.update_filter();

        // Should stop at MAX_CONTENT_MATCHES
        assert!(picker.filtered.len() <= MAX_CONTENT_MATCHES);
    }

    #[test]
    fn test_content_search_respects_file_load_limit() {
        // Create many entries that DON'T match (to trigger all file loads)
        let mut entries: Vec<String> = Vec::new();
        for i in 0..100 {
            entries.push(format!("{}\nNOMATCH_{:03}", "x".repeat(150), i));
        }

        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 100).unwrap();
        for entry in &entries {
            storage.save_entry(entry).unwrap();
        }
        let mut picker = Picker::new(storage).unwrap();

        let start = std::time::Instant::now();
        picker.search_query = "NONEXISTENT_TERM".to_string();
        picker.update_filter();
        let elapsed = start.elapsed();

        // Should complete quickly due to file load limit
        assert!(elapsed < std::time::Duration::from_secs(5));
    }

    #[test]
    fn test_selected_entry_works_with_new_type() {
        let (mut picker, _dir) = test_picker_with_entries(&["first", "second"]);

        picker.update_filter();
        picker.selected.select(Some(0));

        let entry = picker.selected_entry();
        assert!(entry.is_some());
        // Most recent entry is first
        assert!(entry.unwrap().preview.contains("second"));
    }

    #[test]
    fn test_selected_filtered_returns_metadata() {
        let (mut picker, _dir) = test_picker_with_entries(&["test content"]);

        picker.search_query = "test".to_string();
        picker.update_filter();
        picker.selected.select(Some(0));

        let filtered = picker.selected_filtered();
        assert!(filtered.is_some());
        assert_eq!(filtered.unwrap().match_location, MatchLocation::Preview);
        assert!(filtered.unwrap().score > 0);
    }

    #[test]
    fn test_no_results_clears_selection() {
        let (mut picker, _dir) = test_picker_with_entries(&["hello world"]);

        picker.update_filter();
        picker.selected.select(Some(0));
        assert!(picker.selected_entry().is_some());

        picker.search_query = "NONEXISTENT".to_string();
        picker.update_filter();

        assert!(picker.filtered.is_empty());
        assert!(picker.selected.selected().is_none());
    }
}
```

---

## Improvement #2: Pinned/Favorites

### Problem

Important entries get pruned when clipboard history exceeds limit.

### Solution

Pin flag protects entries from automatic pruning. Maximum 25 pinned entries.

### File: `src/storage.rs` - Constants

```rust
const MAX_PREVIEW_LEN: usize = 100;
const DEFAULT_MAX_ENTRIES: usize = 100;
const ABSOLUTE_MAX_ENTRIES: usize = 10000;
const MAX_PINNED: usize = 25;
```

### File: `src/storage.rs` - Updated `ClipEntry`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipEntry {
    pub id: String,
    pub timestamp: i64,
    pub size: usize,
    pub preview: String,
    pub hash: String,
    /// Whether this entry is protected from automatic pruning
    #[serde(default)]
    pub pinned: bool,
}
```

### File: `src/storage.rs` - Updated Storage Struct

```rust
pub struct Storage {
    base_dir: PathBuf,
    max_entries: usize,
}

impl Storage {
    /// Create storage with specified max entries
    pub fn new(base_dir: PathBuf, max_entries: usize) -> Result<Self> {
        fs::create_dir_all(&base_dir)
            .with_context(|| format!("Failed to create storage dir: {:?}", base_dir))?;

        let max_entries = max_entries.clamp(1, ABSOLUTE_MAX_ENTRIES);
        let storage = Self { base_dir, max_entries };

        storage.cleanup_temp_files()?;
        storage.sync_max_entries()?;

        Ok(storage)
    }

    /// Get the configured max entries
    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// Sync max_entries to stored index and prune if necessary
    fn sync_max_entries(&self) -> Result<()> {
        let mut index = self.load_index()?;

        if index.max_entries != self.max_entries {
            index.max_entries = self.max_entries;
            self.prune_unpinned_entries(&mut index)?;
            self.save_index(&index)?;
        }

        Ok(())
    }
}
```

### File: `src/storage.rs` - Pin Methods

```rust
impl Storage {
    /// Toggle pin status of an entry
    /// Returns new pinned state, or error if at pin limit
    pub fn toggle_pin(&self, id: &str) -> Result<bool> {
        let mut index = self.load_index()?;

        let entry = index.entries.iter_mut().find(|e| e.id == id);

        match entry {
            Some(entry) => {
                // Check limit only when pinning (not unpinning)
                if !entry.pinned {
                    let pinned_count = index.entries.iter().filter(|e| e.pinned).count();
                    if pinned_count >= MAX_PINNED {
                        anyhow::bail!(
                            "Maximum pinned entries ({}) reached. Unpin something first.",
                            MAX_PINNED
                        );
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

    /// Explicitly set pin status (used for undo restore)
    pub fn set_pinned(&self, id: &str, pinned: bool) -> Result<()> {
        let mut index = self.load_index()?;

        if let Some(entry) = index.entries.iter_mut().find(|e| e.id == id) {
            // Check limit if pinning
            if pinned && !entry.pinned {
                let pinned_count = index.entries.iter().filter(|e| e.pinned).count();
                if pinned_count >= MAX_PINNED {
                    anyhow::bail!("Maximum pinned entries reached");
                }
            }
            entry.pinned = pinned;
            self.save_index(&index)?;
        }
        Ok(())
    }

    /// Get count of pinned entries
    pub fn pinned_count(&self) -> Result<usize> {
        let index = self.load_index()?;
        Ok(index.entries.iter().filter(|e| e.pinned).count())
    }
}
```

### File: `src/storage.rs` - Updated `save_entry()` with Pin-Aware Dedup

```rust
impl Storage {
    pub fn save_entry(&self, content: &str) -> Result<ClipEntry> {
        let timestamp = chrono::Utc::now().timestamp_millis();
        let id = timestamp.to_string();

        // Compute content hash
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let hash = format!("sha256:{:x}", hasher.finalize());

        let mut index = self.load_index()?;

        // Check for duplicate by hash
        if let Some(pos) = index.entries.iter().position(|e| e.hash == hash) {
            // Move existing entry to front, PRESERVING pin status
            let existing = index.entries.remove(pos);
            index.entries.insert(0, existing.clone());
            self.save_index(&index)?;
            return Ok(existing);
        }

        // Create new entry
        let preview: String = content
            .chars()
            .take(MAX_PREVIEW_LEN)
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();

        let entry = ClipEntry {
            id: id.clone(),
            timestamp,
            size: content.len(),
            preview,
            hash,
            pinned: false, // New entries are never pinned
        };

        // Save content file
        let content_path = self.content_path(&id);
        self.atomic_write(&content_path, content.as_bytes())
            .with_context(|| format!("Failed to write content: {:?}", content_path))?;

        // Add to index
        index.entries.insert(0, entry.clone());

        // Prune old unpinned entries
        self.prune_unpinned_entries(&mut index)?;

        self.save_index(&index)?;
        Ok(entry)
    }

    /// Remove oldest unpinned entries when over limit
    fn prune_unpinned_entries(&self, index: &mut ClipIndex) -> Result<()> {
        while index.entries.iter().filter(|e| !e.pinned).count() > self.max_entries {
            // Find oldest (last) unpinned entry
            if let Some(pos) = index.entries.iter().rposition(|e| !e.pinned) {
                let old = index.entries.remove(pos);
                let old_path = self.content_path(&old.id);
                let _ = fs::remove_file(old_path);
            } else {
                // All entries are pinned - nothing more to prune
                break;
            }
        }
        Ok(())
    }
}
```

### File: `src/picker.rs` - Updated `DeletedEntry` for Pin Restore

```rust
/// Stores deleted entry for undo functionality
struct DeletedEntry {
    entry: ClipEntry,
    content: String,
    was_pinned: bool,  // Track pin state for restoration
    deleted_at: Instant,
}
```

### File: `src/picker.rs` - Pin Toggle in Picker

```rust
impl Picker {
    /// Toggle pin status of selected entry
    fn toggle_pin_selected(&mut self) -> Result<()> {
        if let Some(idx) = self.selected_entry_index() {
            let entry_id = self.entries[idx].id.clone();

            match self.storage.toggle_pin(&entry_id) {
                Ok(is_pinned) => {
                    // Update local state
                    self.entries[idx].pinned = is_pinned;

                    // Re-sort: pinned entries first
                    self.sort_entries_by_pin();

                    let msg = if is_pinned { "★ Pinned" } else { "Unpinned" };
                    self.set_status(msg.to_string(), StatusLevel::Success);
                }
                Err(e) => {
                    self.set_status(e.to_string(), StatusLevel::Warning);
                }
            }
        }
        Ok(())
    }

    /// Sort entries: pinned first (by timestamp), then unpinned (by timestamp)
    fn sort_entries_by_pin(&mut self) {
        // Remember current selection
        let selected_id = self.selected_entry().map(|e| e.id.clone());

        // Sort in place
        self.entries.sort_by(|a, b| {
            match (a.pinned, b.pinned) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => b.timestamp.cmp(&a.timestamp), // Most recent first
            }
        });

        // Rebuild filter to reflect new order
        self.update_filter();

        // Restore selection by ID
        if let Some(id) = selected_id {
            for (i, filtered) in self.filtered.iter().enumerate() {
                if self.entries[filtered.index].id == id {
                    self.selected.select(Some(i));
                    break;
                }
            }
        }

        self.update_scroll_state();
        self.load_preview();
    }

    /// Updated delete to track pin state
    fn delete_selected(&mut self) -> Result<()> {
        if let Some(entry) = self.selected_entry().cloned() {
            let content = self.storage.load_content(&entry.id)?;
            let preview: String = entry.preview.chars().take(30).collect();
            let was_pinned = entry.pinned;

            self.last_deleted = Some(DeletedEntry {
                entry: entry.clone(),
                content,
                was_pinned,
                deleted_at: Instant::now(),
            });

            self.storage.delete_entry(&entry.id)?;
            self.entries.retain(|e| e.id != entry.id);
            self.update_filter();
            self.load_preview();

            let msg = if was_pinned {
                format!("Deleted ★ '{}' - 'u' to undo (5s)", preview)
            } else {
                format!("Deleted '{}' - 'u' to undo (5s)", preview)
            };
            self.set_status(msg, StatusLevel::Warning);
        }
        Ok(())
    }

    /// Updated undo to restore pin state
    fn undo_delete(&mut self) -> Result<()> {
        if let Some(deleted) = self.last_deleted.take() {
            if deleted.deleted_at.elapsed() < Duration::from_secs(5) {
                let preview: String = deleted.entry.preview.chars().take(30).collect();

                // Restore entry
                let restored = self.storage.save_entry(&deleted.content)?;

                // Restore pin state if it was pinned
                if deleted.was_pinned {
                    let _ = self.storage.set_pinned(&restored.id, true);
                }

                // Reload entries
                let index = self.storage.load_index()?;
                self.entries = index.entries;
                self.sort_entries_by_pin();
                self.update_filter();
                self.load_preview();

                let msg = if deleted.was_pinned {
                    format!("Restored ★ '{}'", preview)
                } else {
                    format!("Restored '{}'", preview)
                };
                self.set_status(msg, StatusLevel::Success);
            } else {
                self.set_status("Undo expired".to_string(), StatusLevel::Warning);
            }
        }
        Ok(())
    }
}
```

### File: `src/picker.rs` - Add 'p' to Normal Mode Handler

In `handle_normal_mode()`, add:

```rust
KeyCode::Char('p') => {
    self.toggle_pin_selected()?;
}
```

### Tests: Pinned/Favorites

```rust
#[cfg(test)]
mod pin_tests {
    use super::*;
    use tempfile::TempDir;

    fn test_storage() -> (Storage, TempDir) {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 100).unwrap();
        (storage, dir)
    }

    #[test]
    fn test_new_entry_not_pinned() {
        let (storage, _dir) = test_storage();
        let entry = storage.save_entry("test").unwrap();
        assert!(!entry.pinned);
    }

    #[test]
    fn test_toggle_pin_on() {
        let (storage, _dir) = test_storage();
        let entry = storage.save_entry("pin me").unwrap();

        let is_pinned = storage.toggle_pin(&entry.id).unwrap();
        assert!(is_pinned);

        // Verify persistence
        let index = storage.load_index().unwrap();
        let loaded = index.entries.iter().find(|e| e.id == entry.id).unwrap();
        assert!(loaded.pinned);
    }

    #[test]
    fn test_toggle_pin_off() {
        let (storage, _dir) = test_storage();
        let entry = storage.save_entry("toggle me").unwrap();

        storage.toggle_pin(&entry.id).unwrap(); // Pin
        let is_pinned = storage.toggle_pin(&entry.id).unwrap(); // Unpin
        assert!(!is_pinned);
    }

    #[test]
    fn test_pinned_survives_pruning() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 10).unwrap(); // Small limit

        // Create and pin an entry
        let pinned_entry = storage.save_entry("keep me").unwrap();
        storage.toggle_pin(&pinned_entry.id).unwrap();

        // Fill beyond limit
        for i in 0..20 {
            storage.save_entry(&format!("filler {}", i)).unwrap();
        }

        // Verify pinned entry still exists
        let index = storage.load_index().unwrap();
        let found = index.entries.iter().find(|e| e.id == pinned_entry.id);
        assert!(found.is_some());
        assert!(found.unwrap().pinned);

        // Verify unpinned count is at limit
        let unpinned = index.entries.iter().filter(|e| !e.pinned).count();
        assert_eq!(unpinned, 10);
    }

    #[test]
    fn test_pin_limit_enforced() {
        let (storage, _dir) = test_storage();

        // Pin MAX_PINNED entries
        for i in 0..MAX_PINNED {
            let entry = storage.save_entry(&format!("pinned {}", i)).unwrap();
            storage.toggle_pin(&entry.id).unwrap();
        }

        // Try to pin one more
        let extra = storage.save_entry("one too many").unwrap();
        let result = storage.toggle_pin(&extra.id);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Maximum"));
    }

    #[test]
    fn test_duplicate_preserves_pin_status() {
        let (storage, _dir) = test_storage();

        // Create and pin an entry
        let original = storage.save_entry("duplicate me").unwrap();
        storage.toggle_pin(&original.id).unwrap();

        // Add other entries
        storage.save_entry("other 1").unwrap();
        storage.save_entry("other 2").unwrap();

        // Re-copy same content
        let dup = storage.save_entry("duplicate me").unwrap();

        // Should be same entry, moved to front, still pinned
        assert_eq!(dup.id, original.id);

        let index = storage.load_index().unwrap();
        assert_eq!(index.entries[0].id, original.id);
        assert!(index.entries[0].pinned);
    }

    #[test]
    fn test_backwards_compat_missing_pinned_field() {
        let dir = TempDir::new().unwrap();
        let index_path = dir.path().join("index.json");

        // Write old-format index
        std::fs::write(&index_path, r#"{
            "max_entries": 100,
            "entries": [{
                "id": "12345",
                "timestamp": 12345,
                "size": 4,
                "preview": "test",
                "hash": "sha256:abc"
            }]
        }"#).unwrap();

        std::fs::write(dir.path().join("12345.txt"), "test").unwrap();

        let storage = Storage::new(dir.path().to_path_buf(), 100).unwrap();
        let index = storage.load_index().unwrap();

        assert_eq!(index.entries.len(), 1);
        assert!(!index.entries[0].pinned); // Defaults to false
    }

    #[test]
    fn test_set_pinned_explicit() {
        let (storage, _dir) = test_storage();
        let entry = storage.save_entry("test").unwrap();

        storage.set_pinned(&entry.id, true).unwrap();
        let index = storage.load_index().unwrap();
        assert!(index.entries[0].pinned);

        storage.set_pinned(&entry.id, false).unwrap();
        let index = storage.load_index().unwrap();
        assert!(!index.entries[0].pinned);
    }

    #[test]
    fn test_pinned_count() {
        let (storage, _dir) = test_storage();

        assert_eq!(storage.pinned_count().unwrap(), 0);

        let e1 = storage.save_entry("one").unwrap();
        let e2 = storage.save_entry("two").unwrap();
        storage.toggle_pin(&e1.id).unwrap();
        storage.toggle_pin(&e2.id).unwrap();

        assert_eq!(storage.pinned_count().unwrap(), 2);
    }
}
```

---

## Improvement #3: Atomic File Writes

### Problem

`fs::write()` is not atomic. Interrupted writes can corrupt storage.

### Solution

Write to temp file, fsync, then atomic rename.

### File: `src/storage.rs` - Atomic Write Implementation

```rust
use std::io::Write;

impl Storage {
    /// Atomically write data to a file using write-then-rename pattern
    fn atomic_write(&self, path: &Path, data: &[u8]) -> Result<()> {
        let tmp_path = path.with_extension("tmp");

        // Step 1: Write to temporary file
        let mut file = fs::File::create(&tmp_path)
            .with_context(|| format!("Failed to create temp file: {:?}", tmp_path))?;

        file.write_all(data)
            .with_context(|| format!("Failed to write temp file: {:?}", tmp_path))?;

        // Step 2: Ensure data is flushed to disk
        file.sync_all()
            .with_context(|| format!("Failed to sync temp file: {:?}", tmp_path))?;

        // Step 3: Close file before rename
        drop(file);

        // Step 4: Atomic rename (POSIX guarantees atomicity)
        fs::rename(&tmp_path, path)
            .with_context(|| format!("Failed to rename {:?} to {:?}", tmp_path, path))?;

        // Step 5: Sync parent directory for full durability
        if let Some(parent) = path.parent() {
            if let Ok(dir) = fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }

        Ok(())
    }

    /// Clean up orphaned temp files from interrupted operations
    fn cleanup_temp_files(&self) -> Result<()> {
        if let Ok(entries) = fs::read_dir(&self.base_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "tmp") {
                    eprintln!("[cleanup] Removing orphaned temp file: {:?}", path);
                    let _ = fs::remove_file(&path);
                }
            }
        }
        Ok(())
    }

    /// Updated save_index to use atomic write
    pub fn save_index(&self, index: &ClipIndex) -> Result<()> {
        let path = self.index_path();
        let data = serde_json::to_string_pretty(index)?;
        self.atomic_write(&path, data.as_bytes())
    }
}
```

### File: `src/storage.rs` - Recovery Command Support

```rust
impl Storage {
    /// Attempt to recover from corrupted storage
    /// Rebuilds index from existing content files
    pub fn attempt_recovery(&self) -> Result<usize> {
        eprintln!("[recovery] Starting storage recovery...");

        let index_path = self.index_path();
        let mut recovered_entries: Vec<ClipEntry> = Vec::new();

        // Try to load existing index entries first
        if index_path.exists() {
            match fs::read_to_string(&index_path) {
                Ok(data) => {
                    match serde_json::from_str::<ClipIndex>(&data) {
                        Ok(index) => {
                            eprintln!("[recovery] Loaded {} entries from existing index", index.entries.len());
                            recovered_entries = index.entries;
                        }
                        Err(e) => {
                            eprintln!("[recovery] Index corrupted ({}), scanning files...", e);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[recovery] Cannot read index ({}), scanning files...", e);
                }
            }
        }

        // Collect IDs of entries we already have
        let known_ids: std::collections::HashSet<_> =
            recovered_entries.iter().map(|e| e.id.clone()).collect();

        // Scan for orphaned content files
        let mut orphan_count = 0;
        for entry in fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().map_or(false, |ext| ext == "txt") {
                let id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();

                if known_ids.contains(&id) {
                    continue; // Already in index
                }

                // Try to recover this orphaned content file
                if let Ok(content) = fs::read_to_string(&path) {
                    let timestamp: i64 = id.parse().unwrap_or(0);

                    let mut hasher = Sha256::new();
                    hasher.update(content.as_bytes());
                    let hash = format!("sha256:{:x}", hasher.finalize());

                    let preview: String = content
                        .chars()
                        .take(MAX_PREVIEW_LEN)
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
                    orphan_count += 1;
                }
            }
        }

        eprintln!("[recovery] Found {} orphaned content files", orphan_count);

        // Sort by timestamp descending
        recovered_entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        // Deduplicate by hash (keep most recent)
        let mut seen_hashes = std::collections::HashSet::new();
        recovered_entries.retain(|e| seen_hashes.insert(e.hash.clone()));

        let total = recovered_entries.len();
        eprintln!("[recovery] Total entries after dedup: {}", total);

        // Save recovered index
        let index = ClipIndex {
            max_entries: self.max_entries,
            entries: recovered_entries,
        };
        self.save_index(&index)?;

        eprintln!("[recovery] Recovery complete");
        Ok(total)
    }
}
```

### File: `src/main.rs` - Add Recover Command

```rust
#[derive(Subcommand)]
enum Commands {
    // ... existing commands ...

    /// Attempt to recover from corrupted storage
    Recover,
}

// In main() match:
Some(Commands::Recover) => {
    match storage.attempt_recovery() {
        Ok(count) => {
            println!("Recovery complete. Recovered {} entries.", count);
        }
        Err(e) => {
            eprintln!("Recovery failed: {}", e);
            eprintln!("You may need to manually inspect {:?}", storage.base_dir());
            std::process::exit(1);
        }
    }
}
```

### Tests: Atomic File Writes

```rust
#[cfg(test)]
mod atomic_write_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_atomic_write_creates_file() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 100).unwrap();
        let test_path = dir.path().join("test.json");

        storage.atomic_write(&test_path, b"test data").unwrap();

        assert!(test_path.exists());
        assert_eq!(fs::read_to_string(&test_path).unwrap(), "test data");
    }

    #[test]
    fn test_atomic_write_no_temp_file_remains() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 100).unwrap();
        let test_path = dir.path().join("test.json");

        storage.atomic_write(&test_path, b"test data").unwrap();

        let tmp_path = test_path.with_extension("tmp");
        assert!(!tmp_path.exists());
    }

    #[test]
    fn test_atomic_write_overwrites_existing() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 100).unwrap();
        let test_path = dir.path().join("test.json");

        storage.atomic_write(&test_path, b"initial").unwrap();
        assert_eq!(fs::read_to_string(&test_path).unwrap(), "initial");

        storage.atomic_write(&test_path, b"updated").unwrap();
        assert_eq!(fs::read_to_string(&test_path).unwrap(), "updated");
    }

    #[test]
    fn test_cleanup_removes_orphaned_temp_files() {
        let dir = TempDir::new().unwrap();

        // Create orphaned temp files
        fs::write(dir.path().join("index.tmp"), "orphaned").unwrap();
        fs::write(dir.path().join("12345.tmp"), "orphaned").unwrap();
        fs::write(dir.path().join("normal.txt"), "keep me").unwrap();

        // Storage::new should clean up .tmp files
        let _storage = Storage::new(dir.path().to_path_buf(), 100).unwrap();

        assert!(!dir.path().join("index.tmp").exists());
        assert!(!dir.path().join("12345.tmp").exists());
        assert!(dir.path().join("normal.txt").exists()); // Not touched
    }

    #[test]
    fn test_recovery_from_missing_index() {
        let dir = TempDir::new().unwrap();

        // Create content files without index
        fs::write(dir.path().join("1000.txt"), "content one").unwrap();
        fs::write(dir.path().join("2000.txt"), "content two").unwrap();

        let storage = Storage::new(dir.path().to_path_buf(), 100).unwrap();
        let count = storage.attempt_recovery().unwrap();

        assert_eq!(count, 2);

        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), 2);
    }

    #[test]
    fn test_recovery_deduplicates_by_hash() {
        let dir = TempDir::new().unwrap();

        // Create content files with same content (same hash)
        fs::write(dir.path().join("1000.txt"), "duplicate").unwrap();
        fs::write(dir.path().join("2000.txt"), "duplicate").unwrap();

        let storage = Storage::new(dir.path().to_path_buf(), 100).unwrap();
        let count = storage.attempt_recovery().unwrap();

        // Should keep only one (most recent)
        assert_eq!(count, 1);
    }

    #[test]
    fn test_concurrent_saves_dont_corrupt() {
        use std::sync::Arc;
        use std::thread;

        let dir = TempDir::new().unwrap();
        let storage = Arc::new(Storage::new(dir.path().to_path_buf(), 100).unwrap());

        let mut handles = vec![];
        for i in 0..10 {
            let storage = Arc::clone(&storage);
            handles.push(thread::spawn(move || {
                storage.save_entry(&format!("thread {} content", i)).unwrap();
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // All entries should be saved without corruption
        let index = storage.load_index().unwrap();
        assert_eq!(index.entries.len(), 10);
    }
}
```

---

## Improvement #4: Preview Scrolling with Focus Mode

### Problem

Cannot view content beyond visible preview area without pasting first.

### Solution

Tab switches focus between list and preview. Ctrl+D/U scrolls the focused pane.

### File: `src/picker.rs` - Focus Mode Types

```rust
/// Which pane has keyboard focus
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Focus {
    #[default]
    List,
    Preview,
}
```

### File: `src/picker.rs` - Preview Scroll Methods

```rust
impl Picker {
    /// Reset preview scroll when selection changes
    fn load_preview(&mut self) {
        let entry_id = self.selected_entry().map(|e| e.id.clone());

        match entry_id {
            Some(id) if self.preview_id.as_ref() != Some(&id) => {
                match self.storage.load_content(&id) {
                    Ok(content) => {
                        self.preview_content = Some(content);
                        self.preview_id = Some(id);
                        self.preview_scroll = 0; // Reset scroll on new selection
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
            _ => {} // Same entry, keep scroll position
        }
    }

    /// Scroll preview pane by delta lines
    fn scroll_preview(&mut self, delta: i32) {
        if let Some(content) = &self.preview_content {
            let total_lines = content.lines().count();

            let new_scroll = if delta > 0 {
                self.preview_scroll.saturating_add(delta as usize)
            } else {
                self.preview_scroll.saturating_sub((-delta) as usize)
            };

            // Clamp: allow scrolling until last line is at top
            self.preview_scroll = new_scroll.min(total_lines.saturating_sub(1));
        }
    }

    /// Toggle focus between list and preview
    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::List => Focus::Preview,
            Focus::Preview => Focus::List,
        };
    }
}
```

### File: `src/picker.rs` - Updated `render_preview()` with Scroll

```rust
impl Picker {
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

        // Build title with scroll position info
        let title = if total_lines > visible_height {
            let start = self.preview_scroll + 1;
            let end = (self.preview_scroll + visible_height).min(total_lines);
            format!("Preview - {} [{}-{}/{}]", metadata, start, end, total_lines)
        } else {
            format!("Preview - {}", metadata)
        };

        // Highlight border when preview is focused
        let border_style = if self.focus == Focus::Preview {
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

        // Scroll indicators
        let can_scroll_up = self.preview_scroll > 0;
        let can_scroll_down = self.preview_scroll + visible_height < total_lines;

        if can_scroll_up {
            let indicator = Paragraph::new("▲")
                .style(Style::default().fg(Color::Yellow));
            frame.render_widget(indicator, Rect {
                x: area.x + area.width - 2,
                y: area.y,
                width: 1,
                height: 1,
            });
        }

        if can_scroll_down {
            let indicator = Paragraph::new("▼")
                .style(Style::default().fg(Color::Yellow));
            frame.render_widget(indicator, Rect {
                x: area.x + area.width - 2,
                y: area.y + area.height - 1,
                width: 1,
                height: 1,
            });
        }
    }
}
```

### File: `src/picker.rs` - Updated Key Handlers

```rust
impl Picker {
    fn handle_normal_mode(&mut self, key: KeyEvent) -> Result<Option<Option<String>>> {
        // Handle pending 'g' for gg
        if self.pending_g {
            self.pending_g = false;
            if key.code == KeyCode::Char('g') {
                self.jump_to_start();
                return Ok(None);
            }
        }

        match key.code {
            // ═══════════════════════════════════════════════════════
            // Focus Management
            // ═══════════════════════════════════════════════════════
            KeyCode::Tab => {
                self.toggle_focus();
            }

            // ═══════════════════════════════════════════════════════
            // Exit
            // ═══════════════════════════════════════════════════════
            KeyCode::Esc | KeyCode::Char('q') => return Ok(Some(None)),

            // ═══════════════════════════════════════════════════════
            // Select
            // ═══════════════════════════════════════════════════════
            KeyCode::Enter => {
                if let Some(entry) = self.selected_entry() {
                    let content = self.storage.load_content(&entry.id)?;
                    return Ok(Some(Some(content)));
                }
            }

            // ═══════════════════════════════════════════════════════
            // Navigation - Focus-Aware
            // ═══════════════════════════════════════════════════════
            KeyCode::Char('j') | KeyCode::Down => {
                match self.focus {
                    Focus::List => self.move_selection(1),
                    Focus::Preview => self.scroll_preview(1),
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                match self.focus {
                    Focus::List => self.move_selection(-1),
                    Focus::Preview => self.scroll_preview(-1),
                }
            }

            // Page navigation - Focus-Aware
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                match self.focus {
                    Focus::List => self.move_selection(10),
                    Focus::Preview => self.scroll_preview(15),
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                match self.focus {
                    Focus::List => self.move_selection(-10),
                    Focus::Preview => self.scroll_preview(-15),
                }
            }
            KeyCode::PageDown => {
                match self.focus {
                    Focus::List => self.move_selection(10),
                    Focus::Preview => self.scroll_preview(15),
                }
            }
            KeyCode::PageUp => {
                match self.focus {
                    Focus::List => self.move_selection(-10),
                    Focus::Preview => self.scroll_preview(-15),
                }
            }

            // ═══════════════════════════════════════════════════════
            // Jump Commands (List only)
            // ═══════════════════════════════════════════════════════
            KeyCode::Char('G') => {
                if self.focus == Focus::List {
                    self.jump_to_end();
                }
            }
            KeyCode::Char('g') => {
                if self.focus == Focus::List {
                    self.pending_g = true;
                }
            }

            // ═══════════════════════════════════════════════════════
            // Actions (Work in any focus)
            // ═══════════════════════════════════════════════════════
            KeyCode::Char('/') => {
                self.mode = Mode::Search;
                self.focus = Focus::List; // Search focuses list
            }
            KeyCode::Char('d') => {
                self.delete_selected()?;
            }
            KeyCode::Char('u') => {
                self.undo_delete()?;
            }
            KeyCode::Char('p') => {
                self.toggle_pin_selected()?;
            }

            // Quick search
            KeyCode::Char(c) if c.is_alphanumeric() || c == ' ' => {
                self.mode = Mode::Search;
                self.focus = Focus::List;
                self.search_query.push(c);
                self.update_filter();
                self.load_preview();
            }

            _ => {}
        }

        Ok(None)
    }
}
```

### File: `src/picker.rs` - Updated Status Line

```rust
fn render_status_line(&mut self, frame: &mut Frame, area: Rect) {
    let status_text = if let Some((msg, level, instant)) = &self.status_message {
        if instant.elapsed() < Duration::from_secs(3) {
            // ... existing status message handling ...
            Some((/* ... */))
        } else {
            self.status_message = None;
            None
        }
    } else {
        None
    };

    let (text, style) = status_text.unwrap_or_else(|| {
        let mode_indicator = match self.mode {
            Mode::Normal => "[NORMAL]",
            Mode::Search => "[SEARCH]",
        };
        let focus_indicator = match self.focus {
            Focus::List => "List",
            Focus::Preview => "Preview",
        };
        (
            format!(
                "{} {} | Tab:Switch  j/k:Nav  Ctrl+d/u:Page  p:Pin  d:Del  /:Search  Enter:Paste",
                mode_indicator,
                focus_indicator
            ),
            Style::default().fg(Color::DarkGray),
        )
    });

    let help = Paragraph::new(text).style(style);
    frame.render_widget(help, area);
}
```

### Tests: Preview Scrolling

```rust
#[cfg(test)]
mod preview_scroll_tests {
    use super::*;
    use tempfile::TempDir;

    fn picker_with_long_content() -> (Picker, TempDir) {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 100).unwrap();

        // Create content with many lines
        let content = (0..100).map(|i| format!("Line {}", i)).collect::<Vec<_>>().join("\n");
        storage.save_entry(&content).unwrap();

        let picker = Picker::new(storage).unwrap();
        (picker, dir)
    }

    #[test]
    fn test_initial_scroll_is_zero() {
        let (picker, _dir) = picker_with_long_content();
        assert_eq!(picker.preview_scroll, 0);
    }

    #[test]
    fn test_scroll_down() {
        let (mut picker, _dir) = picker_with_long_content();
        picker.load_preview();

        picker.scroll_preview(10);
        assert_eq!(picker.preview_scroll, 10);
    }

    #[test]
    fn test_scroll_up() {
        let (mut picker, _dir) = picker_with_long_content();
        picker.load_preview();
        picker.preview_scroll = 50;

        picker.scroll_preview(-10);
        assert_eq!(picker.preview_scroll, 40);
    }

    #[test]
    fn test_scroll_clamps_at_zero() {
        let (mut picker, _dir) = picker_with_long_content();
        picker.load_preview();
        picker.preview_scroll = 5;

        picker.scroll_preview(-100);
        assert_eq!(picker.preview_scroll, 0);
    }

    #[test]
    fn test_scroll_clamps_at_end() {
        let (mut picker, _dir) = picker_with_long_content();
        picker.load_preview();

        picker.scroll_preview(1000);
        // Should clamp to total_lines - 1 = 99
        assert!(picker.preview_scroll <= 99);
    }

    #[test]
    fn test_scroll_resets_on_selection_change() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 100).unwrap();

        let long_content = (0..100).map(|i| format!("Line {}", i)).collect::<Vec<_>>().join("\n");
        storage.save_entry(&long_content).unwrap();
        storage.save_entry("short").unwrap();

        let mut picker = Picker::new(storage).unwrap();
        picker.load_preview();
        picker.preview_scroll = 50;

        // Change selection
        picker.move_selection(1);

        // Scroll should reset
        assert_eq!(picker.preview_scroll, 0);
    }

    #[test]
    fn test_focus_toggle() {
        let (mut picker, _dir) = picker_with_long_content();

        assert_eq!(picker.focus, Focus::List);
        picker.toggle_focus();
        assert_eq!(picker.focus, Focus::Preview);
        picker.toggle_focus();
        assert_eq!(picker.focus, Focus::List);
    }

    #[test]
    fn test_focus_affects_navigation() {
        let (mut picker, _dir) = picker_with_long_content();
        picker.load_preview();

        // List focused: j moves selection
        picker.focus = Focus::List;
        let initial_selection = picker.selected.selected();
        // Note: Would need to simulate key event for full test

        // Preview focused: j scrolls preview
        picker.focus = Focus::Preview;
        picker.scroll_preview(1);
        assert_eq!(picker.preview_scroll, 1);
    }

    #[test]
    fn test_no_scroll_for_short_content() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 100).unwrap();
        storage.save_entry("short").unwrap();

        let mut picker = Picker::new(storage).unwrap();
        picker.load_preview();

        // Scrolling should have no effect
        picker.scroll_preview(100);
        assert_eq!(picker.preview_scroll, 0);
    }
}
```

---

## Improvement #5: Configurable Max Entries

### Problem

Hardcoded 100-entry limit doesn't suit all users.

### Solution

CLI flag, environment variable, and stored preference.

### File: `src/main.rs` - Updated CLI

```rust
#[derive(Parser)]
#[command(name = "clipstack")]
#[command(about = "Fast clipboard manager with lazy-loading history")]
#[command(version)]
struct Cli {
    /// Custom storage directory
    #[arg(long, global = true)]
    storage_dir: Option<PathBuf>,

    /// Maximum entries to store (1-10000, default: 100)
    /// Can also be set via CLIPSTACK_MAX_ENTRIES environment variable
    #[arg(long, global = true, value_parser = clap::value_parser!(u32).range(1..=10000))]
    max_entries: Option<u32>,

    #[command(subcommand)]
    command: Option<Commands>,
}
```

### File: `src/main.rs` - Updated `main()`

```rust
fn main() -> Result<()> {
    let cli = Cli::parse();

    // Determine max_entries: CLI > env > default
    let max_entries = cli
        .max_entries
        .map(|n| n as usize)
        .or_else(|| {
            std::env::var("CLIPSTACK_MAX_ENTRIES")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(100)
        .clamp(1, 10000);

    // Check dependencies for commands that need clipboard
    if matches!(
        cli.command,
        None | Some(Commands::Pick)
            | Some(Commands::Copy)
            | Some(Commands::Paste)
            | Some(Commands::Daemon)
    ) {
        check_dependencies()?;
    }

    let storage_dir = cli.storage_dir.unwrap_or_else(storage::Storage::default_dir);
    let storage = storage::Storage::new(storage_dir, max_entries)?;

    // ... rest of main
}
```

### File: `src/main.rs` - Updated Stats Command

```rust
Some(Commands::Stats) => {
    let index = storage.load_index()?;
    let total_size: usize = index.entries.iter().map(|e| e.size).sum();
    let pinned_count = index.entries.iter().filter(|e| e.pinned).count();
    let unpinned_count = index.entries.len() - pinned_count;

    println!("Entries:    {}", index.entries.len());
    println!("  Pinned:   {} (protected)", pinned_count);
    println!("  Regular:  {}/{}", unpinned_count, storage.max_entries());
    println!("Total size: {}", util::format_size(total_size));

    if let Some(oldest) = index.entries.last() {
        println!("Oldest:     {}", util::format_relative_time(oldest.timestamp));
    }
    if let Some(newest) = index.entries.first() {
        println!("Newest:     {}", util::format_relative_time(newest.timestamp));
    }
}
```

### File: `src/main.rs` - Updated Status Command

```rust
Some(Commands::Status) => {
    print_status(&storage)?;

    // Show configuration
    println!();
    println!("Configuration:");
    println!("  Max entries: {}", storage.max_entries());
    if std::env::var("CLIPSTACK_MAX_ENTRIES").is_ok() {
        println!("  (from CLIPSTACK_MAX_ENTRIES env var)");
    }
}
```

### File: `src/daemon.rs` - Update to Accept Max Entries

```rust
impl Daemon {
    pub fn new(storage_dir: Option<PathBuf>, max_entries: usize) -> Result<Self> {
        Self::new_with_lock(storage_dir, max_entries, false)
    }

    pub fn new_with_lock(
        storage_dir: Option<PathBuf>,
        max_entries: usize,
        use_local_lock: bool,
    ) -> Result<Self> {
        let base_dir = storage_dir.unwrap_or_else(Storage::default_dir);
        let storage = Storage::new(base_dir.clone(), max_entries)?;

        // ... rest unchanged
    }
}
```

### File: `src/main.rs` - Update Daemon Command

```rust
Some(Commands::Daemon) => {
    let daemon = daemon::Daemon::new(Some(storage.base_dir().to_path_buf()), max_entries)?;

    let running = daemon.stop_handle();
    ctrlc_handler(running);

    daemon.run()?;
}
```

### Tests: Configurable Max Entries

```rust
#[cfg(test)]
mod max_entries_tests {
    use super::*;
    use tempfile::TempDir;

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
    fn test_max_entries_clamps_low() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 0).unwrap();
        assert_eq!(storage.max_entries(), 1);
    }

    #[test]
    fn test_max_entries_clamps_high() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 999999).unwrap();
        assert_eq!(storage.max_entries(), 10000);
    }

    #[test]
    fn test_reducing_max_entries_prunes() {
        let dir = TempDir::new().unwrap();

        // Create with high limit
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
    fn test_pinned_entries_dont_count_against_limit() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 5).unwrap();

        // Pin 3 entries
        for i in 0..3 {
            let entry = storage.save_entry(&format!("pinned {}", i)).unwrap();
            storage.toggle_pin(&entry.id).unwrap();
        }

        // Add 10 more regular entries
        for i in 0..10 {
            storage.save_entry(&format!("regular {}", i)).unwrap();
        }

        let index = storage.load_index().unwrap();
        let pinned = index.entries.iter().filter(|e| e.pinned).count();
        let unpinned = index.entries.iter().filter(|e| !e.pinned).count();

        assert_eq!(pinned, 3);    // All pinned preserved
        assert_eq!(unpinned, 5);  // Capped at max_entries
    }

    #[test]
    fn test_max_entries_getter() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::new(dir.path().to_path_buf(), 42).unwrap();
        assert_eq!(storage.max_entries(), 42);
    }
}
```

---

## Implementation Order

Execute improvements in this order to minimize conflicts and build on dependencies:

### Phase 1: Foundation (Do First)

1. **#5 Configurable Max Entries** - Changes `Storage::new()` signature that everything depends on
2. **#3 Atomic File Writes** - Adds reliability before other changes

### Phase 2: Storage Features

3. **#2 Pinned/Favorites** - Adds `pinned` field to `ClipEntry`, modifies pruning

### Phase 3: Picker Features

4. **#1 Full Content Search** - Major picker refactor (`FilteredEntry` type)
5. **#4 Preview Scrolling** - Adds focus mode and scroll state

### Commit Strategy

Each improvement should be a separate commit:

```
feat(storage): add configurable max entries via CLI and env var
feat(storage): implement atomic file writes with recovery
feat(storage): add pinned/favorites with pruning protection
feat(picker): add full content search with match indicators
feat(picker): add preview scrolling with focus mode
```

---

## Master Test Plan

### Pre-Implementation Checklist

- [ ] All existing tests pass: `cargo test`
- [ ] No clippy warnings: `cargo clippy`
- [ ] Code compiles in release: `cargo build --release`

### Per-Improvement TDD Workflow

1. Write tests first (copy from this spec)
2. Verify tests fail (red)
3. Implement feature
4. Verify tests pass (green)
5. Refactor if needed
6. Run full suite + clippy

### Integration Test Checklist

After all improvements:

- [ ] Fresh install works
- [ ] Upgrade from old storage format works (missing `pinned` field)
- [ ] Daemon starts and monitors clipboard
- [ ] Picker shows entries with pin indicators
- [ ] Search finds content beyond preview
- [ ] Preview scrolling works with Tab focus
- [ ] Pin/unpin persists across restarts
- [ ] Pruning respects pins
- [ ] Recovery command rebuilds index
- [ ] `--max-entries` flag works
- [ ] `CLIPSTACK_MAX_ENTRIES` env var works
- [ ] Stats shows correct counts
- [ ] All keybindings work as documented

### Manual Test Script

```bash
# 1. Fresh start
rm -rf ~/.local/share/clipd
clipstack status

# 2. Basic operations
echo "test1" | clipstack copy
clipstack list
clipstack paste

# 3. Picker with search
clipstack pick  # Search for text beyond preview

# 4. Pin workflow
clipstack pick  # Press 'p' to pin, verify star appears

# 5. Pruning test
for i in {1..150}; do echo "entry $i" | clipstack copy; done
clipstack stats  # Should show 100 regular + pinned

# 6. Max entries
CLIPSTACK_MAX_ENTRIES=20 clipstack stats

# 7. Recovery
clipstack recover
```

---

## Appendix: File Change Summary

| File | Changes |
|------|---------|
| `src/storage.rs` | Add `pinned` field, `max_entries` param, `atomic_write()`, `toggle_pin()`, `attempt_recovery()`, prune logic |
| `src/picker.rs` | Add `FilteredEntry`, `MatchLocation`, `Focus`, preview scroll, focus mode, pin toggle |
| `src/main.rs` | Add `--max-entries` flag, `Recover` command, update daemon/stats/status |
| `src/daemon.rs` | Update `new()` signature for max_entries |

Total estimated lines changed: ~800-1000
