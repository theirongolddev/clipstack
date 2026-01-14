# ClipStack Feature Implementation Plan

## Overview

This document specifies features to implement in the ClipStack TUI clipboard manager.

**Development Approach**: TDD (Test-Driven Development)
- RED: Write failing test first
- GREEN: Implement minimal code to pass
- REFACTOR: Clean up while tests stay green

**Design Philosophy**: Polished/Premium aesthetic (Stripe-like refinement)
- Power-user productivity focus
- Code snippet special treatment
- Image support roadmap

---

# Cross-cutting UI States (applies to all phases)

Premium TUIs are defined less by color and more by calm, explicit state transitions. This section establishes patterns used throughout all features.

## Empty States
- **No history yet**: Explain how to copy and open ClipStack
- **No matches**: Show "0 results" plus quick tips (clear query, switch regex, widen time filter)

## Loading States
- **Preview loading (async)**: Show subtle spinner + "Loading preview..."
- **Deep search in progress**: Show "Searching content..." badge

## Error States
- **Storage read error**: Non-fatal overlay with retry hint
- **Clipboard copy error**: Transient banner + actionable message
- **Invalid regex**: Inline error in search box and a non-panicking empty list

### TDD Additions
- `test_invalid_regex_does_not_panic`
- `test_storage_error_renders_overlay_and_recovers`
- `test_empty_state_shows_helpful_message`

---

# PHASE 0: Architecture & Performance Guardrails (Pre-work)

## Goal
Ensure all interactive operations remain responsive as history grows:
- No blocking disk reads on the render thread
- Search and preview loading can be incremental and cancellable

## Non-Functional Performance Budgets (enforced)
- Input-to-state-update (key handling): p95 <= 1ms
- Render (ratatui draw call): p95 <= 8ms (target 60fps headroom)
- Query update to first visible results: <= 30ms
- Deep search time-slice per tick: <= 5-15ms (configurable)
- Cold start (open picker, show list): <= 120ms for 5k entries

## Observability (dev + CI)
- Add a minimal instrumentation layer (feature-flagged in release):
  - timings: render, filter, search tick, preview load
  - counters: cache hits/misses, cancelled searches, stale drops
- Add a micro-benchmark harness for:
  - filter pipeline on 1k/5k/20k synthetic entries
  - highlight pipeline on worst-case lines (wide + many matches)
- CI gate: fail if budgets regress beyond tolerance on synthetic tests

## Architectural Changes (enablers for Phases 1-5)

### 0.0 Storage Correctness Contract (must define before feature work)
- **Single-writer lock**: prevent two ClipStack instances from mutating history concurrently.
  - Use a lock file with PID + timestamp; fail with actionable UI if locked.
- **Atomic writes**:
  - Write new entry to temp file, fsync, then atomic rename into place.
  - Maintain an append-only index (or manifest) updated via atomic rename.
- **Versioned storage**:
  - Persist `storage_version` and `entry_schema_version`.
  - Provide forward migration path; unknown future versions show a clear error.
- **Corruption strategy**:
  - Validate length + checksum (CRC32 or xxhash) per entry blob.
  - Corrupt entries are quarantined and shown as "[corrupt]" (not panics).

#### TDD Additions (storage)
- `test_storage_lock_prevents_double_writer`
- `test_atomic_write_survives_crash_simulation` (write temp, abort before rename)
- `test_corrupt_entry_quarantined_not_panics`
- `test_storage_version_mismatch_renders_error`

### 0.1 Separate "Domain" from "UI State"
- **Domain**: entries metadata, storage access, clip content types
- **UI State**: selection, scroll, mode, overlays, query, transient banners
- Introduce a small "ViewModel" layer for:
  - filter pipeline (time filter + query filter + sort)
  - derived UI flags (has_unread, match counts, etc.)

### 0.2 Introduce Asynchronous Content Loading
- Add a lightweight background worker (single thread is sufficient initially) with a strict protocol:
  - All requests include `{ request_id, generation, priority }`
  - UI maintains `active_generation` per concern (preview/search) and drops stale responses
  - Worker executes via priority queue to preserve responsiveness
  - Cancellation is cooperative via `CancellationToken` checked per chunk
  - Backpressure: bounded channels; UI coalesces redundant requests

  **Requests:**
  - `LoadContent { entry_id, request_id, generation }` (HIGH priority)
  - `SearchContentTick { query, mode, budget_ms, exclude_ids, request_id, generation }` (MED priority)
  - `ComputeHighlights { entry_id, visible_range, query, request_id, generation }` (LOW/optional priority)

  **Responses:**
  - `ContentLoaded { entry_id, content, request_id, generation }`
  - `SearchMatches { matches, request_id, generation }`
  - `HighlightsReady { entry_id, spans, request_id, generation }`

- UI shows partial results and a subtle "searching..." affordance while deep search completes

### 0.3 Add Caching (bounded memory)
- LRU cache for loaded content by `entry_id` (and optionally by `content_hash`):
  - Avoids repeated disk reads during search + preview
- Optional cache for "render-ready lines" (highlighted spans) keyed by `(entry_id, query, scroll_window)`

### 0.4 Add Cancellation + Debouncing
- Debounce query updates (e.g. 30-75ms) to avoid re-filtering on every keystroke
- Cancel in-flight deep searches when query changes

### TDD Additions
- Add unit tests for filter pipeline determinism and selection invariants
- `test_debounce_prevents_rapid_refilter`
- `test_cancel_stale_search_requests`
- `test_worker_priority_preempts_long_search_tick`
- `test_bounded_channel_coalesces_requests`

---

# PHASE 1: Core Functionality (Features 1-4)

## Feature 1: Stricter Search Filtering

### Problem
Fuzzy matching via `SkimMatcherV2` is too permissive. Query "abc" matches "a_big_car" because the characters a-b-c appear in order, even though they're scattered. Users expect closer matches only.

### Current Behavior
- `filter_entries()` accepts any match with `Some(score)`
- All fuzzy matches are included regardless of score

### Solution
Replace "score threshold only" with a match-quality model:
1. Classify match type: ExactSubstring, Prefix, WordBoundary, TightFuzzy, ScatteredFuzzy
2. Drop ScatteredFuzzy for queries >= N characters unless explicitly allowed
3. Rank primarily by class, secondarily by fuzzy score
4. Add Smartcase: uppercase in query => case-sensitive; otherwise case-insensitive

This yields predictable relevance and reduces constant-tuning.

### Implementation

**Add constants (configurable via config/env for power-user tuning):**
```rust
/// Minimum query length to apply strict scattered-match filtering.
const MIN_QUERY_LEN_STRICT: usize = 3;

/// Drop scattered fuzzy matches when query is long enough (unless configured)
const DROP_SCATTERED_FOR_LEN_AT_LEAST: usize = 3;

/// Boost for word-boundary matches (e.g., camelCase, snake_case boundaries).
const WORD_BOUNDARY_BOOST: i64 = 250;

/// Boost exact substring matches to the top even when fuzzy scores are close.
const SUBSTRING_BOOST: i64 = 500;

/// Match quality classification for stable ranking.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MatchClass {
    ExactSubstring = 0,  // Highest priority
    Prefix = 1,
    WordBoundary = 2,
    TightFuzzy = 3,
    ScatteredFuzzy = 4,  // Lowest priority (often dropped)
}
```

**Modify `filter_entries()`:**

```rust
fn filter_entries(&self, query: &str) -> Vec<FilteredEntry> {
    // Determine case mode (Smartcase)
    // - if query has any uppercase => case-sensitive
    // - else case-insensitive match checks
    let case_sensitive = query.chars().any(|c| c.is_uppercase());
    let query_lower = query.to_lowercase();
    let mut results: Vec<FilteredEntry> = Vec::new();

    // Phase 1: Search previews (always available, fast)
    for (idx, entry) in self.entries.iter().enumerate() {
        let preview = if case_sensitive {
            &entry.preview
        } else {
            &entry.preview.to_lowercase()
        };
        let match_query = if case_sensitive { query } else { &query_lower };

        // Classify match quality first, then score
        // Exact substring / prefix / word-boundary checks are cheap and stable.
        // Fuzzy match is fallback.
        let (match_class, score) = self.classify_match(preview, match_query, &entry.preview);
        
        // Drop scattered fuzzy matches when query is long enough (unless configured)
        if match_class == MatchClass::ScatteredFuzzy 
            && query.len() >= DROP_SCATTERED_FOR_LEN_AT_LEAST {
            continue;
        }

        if let Some(s) = score {
            results.push(FilteredEntry {
                index: idx,
                score: s,
                match_class,
                match_location: MatchLocation::Preview,
            });
        }
    }

    // Phase 2 (revised): Deep content search is incremental and budgeted.
    // - UI immediately shows preview matches.
    // - Background worker searches content for additional matches with a per-tick budget.
    // - Results stream in and are merged/deduped by entry_id.
    //
    // Rationale: prevents blocking and scales with history size.

    // Sort by match class first, then by score within class
    results.sort_by(|a, b| (a.match_class, b.score).cmp(&(b.match_class, a.score)));
    results
}

fn classify_match(&self, text: &str, query: &str, original: &str) -> (MatchClass, Option<i64>) {
    // Check exact substring first (highest quality)
    if text.contains(query) {
        let score = self.matcher.fuzzy_match(original, query)
            .map(|s| s + SUBSTRING_BOOST)
            .or(Some(SUBSTRING_BOOST));
        return (MatchClass::ExactSubstring, score);
    }

    // Check prefix match
    if text.starts_with(query) {
        let score = self.matcher.fuzzy_match(original, query)
            .map(|s| s + SUBSTRING_BOOST);
        return (MatchClass::Prefix, score);
    }

    // Check word boundary match (camelCase, snake_case, etc.)
    if self.matches_word_boundary(text, query) {
        let score = self.matcher.fuzzy_match(original, query)
            .map(|s| s + WORD_BOUNDARY_BOOST);
        return (MatchClass::WordBoundary, score);
    }

    // Fall back to fuzzy matching
    if let Some(score) = self.matcher.fuzzy_match(original, query) {
        // Distinguish tight vs scattered based on score density
        let class = if score >= (query.len() as i64 * 20) {
            MatchClass::TightFuzzy
        } else {
            MatchClass::ScatteredFuzzy
        };
        return (class, Some(score));
    }

    (MatchClass::ScatteredFuzzy, None)
}
```

### Deep Search Implementation Notes
- Add a background request: `SearchContent { query, min_score, budget_ms, exclude_ids }`
- Budget defaults: 5-15ms per UI tick; cancels on query change
- Deep matches display a subtle "[content]" tag as planned

### Deep Search Acceleration (optional but recommended)
- Add per-entry "candidate rejection" metadata computed at ingest:
  - normalized lowercase preview tokens (bounded)
  - optional trigram bloom filter for content (very small, fast reject)
- If rejection says "impossible," skip loading/scanning content entirely.
- Optional background index build:
  - builds over time; never blocks UI; can be disabled via config.

### Tuning
- `WORD_BOUNDARY_BOOST = 250`: Moderate boost for camelCase/snake_case matches
- `SUBSTRING_BOOST = 500`: Strong boost for exact substring matches
- `DROP_SCATTERED_FOR_LEN_AT_LEAST = 3`: Drop scattered matches for 3+ char queries

### Edge Cases
- Empty query: bypasses filtering entirely (handled in `update_filter`)
- Single-char query: uses gentler threshold to avoid over-filtering
- Smartcase: queries with uppercase force case-sensitive matching

### TDD Test Cases
Add to `mod tests` in picker.rs:
- `test_match_class_exact_substring_beats_fuzzy`: exact substring ranks higher than scattered
- `test_match_class_word_boundary_beats_scattered`: word boundary matches rank higher
- `test_scattered_dropped_for_len_ge_threshold`: "abc" should NOT match "a_big_car"
- `test_smartcase_case_sensitive_when_query_has_uppercase`: "Test" is case-sensitive
- `test_strict_search_accepts_substring`: "test" SHOULD match "testing"
- `test_strict_search_accepts_close_fuzzy`: "tst" SHOULD match "test"
- `test_strict_search_single_char_is_permissive`: short queries don't over-filter
- `test_substring_boost_ranks_higher`: exact substring outranks scattered fuzzy
- `test_deep_search_is_incremental`: preview results appear before content results
- `test_deep_search_cancels_on_query_change`: ensures no stale results are applied

### Manual Testing
1. Search "abc", verify items with scattered a-b-c no longer match
2. Search "test", verify "testing" and "test_file" still match
3. Verify title shows accurate filtered count (e.g., "3/50 matching")

---

## Feature 2: Ctrl+d/u Scrolling in Preview Mode

### Problem
When focused in the content pane (Tab to enter), only `j/k` work for line-by-line scrolling. Users expect `Ctrl+d/u` for half-page scrolling (vim convention).

### Current Behavior
- `handle_preview_mode()` handles j/k, PageUp/PageDown, g/G
- `Ctrl+d/u` are only handled in `handle_normal_mode()` for list navigation

### Solution
Add `Ctrl+d/u` (half-page) and `Ctrl+f/b` (full-page) handlers in preview mode.

### Implementation

**Add to `handle_preview_mode()`:**

```rust
// Half-page scrolling (Ctrl+d/u - vim style)
KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
    let half_page = (self.preview_height as usize) / 2;
    let max_scroll = self.max_preview_scroll();
    self.preview_scroll = (self.preview_scroll + half_page).min(max_scroll);
}
KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
    let half_page = (self.preview_height as usize) / 2;
    self.preview_scroll = self.preview_scroll.saturating_sub(half_page);
}

// Full page scrolling (Ctrl+f/b)
KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
    let page = self.preview_height as usize;
    let max_scroll = self.max_preview_scroll();
    self.preview_scroll = (self.preview_scroll + page).min(max_scroll);
}
KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
    let page = self.preview_height as usize;
    self.preview_scroll = self.preview_scroll.saturating_sub(page);
}
```

**Update help text in `render_status_line()`:**

Change:
```rust
"[PREVIEW] j/k:Scroll  PgUp/Dn:Page  g/G:Top/Bottom  Tab/Esc:Back  q:Quit"
```

To:
```rust
"[PREVIEW] j/k:Line  ^d/u:Half  ^f/b:Page  g/G:Top/Bot  Tab/Esc:Back  q:Quit"
```

### Edge Cases
- Zero/small `preview_height`: half-page becomes 0-1 lines, degrades gracefully to line scroll
- Already at bounds: `saturating_sub` and `.min(max_scroll)` prevent over-scroll

### TDD Test Cases
Add to `mod tests` in picker.rs:
- `test_preview_scroll_ctrl_d_moves_half_page`: verify scroll increases by height/2
- `test_preview_scroll_ctrl_u_moves_half_page`: verify scroll decreases by height/2
- `test_preview_scroll_respects_max_bounds`: Ctrl+d at near-end stops at max
- `test_preview_scroll_respects_min_bounds`: Ctrl+u at start stays at 0

### Manual Testing
1. Tab into preview pane with long content
2. Press `Ctrl+d` - verify scrolls down half a page
3. Press `Ctrl+u` - verify scrolls up half a page
4. Press `Ctrl+f` - verify scrolls down full page
5. Press `Ctrl+b` - verify scrolls up full page
6. Verify scrolling respects bounds (no scroll past start/end)

---

## Feature 3: Highlight Search Matches in Content Pane

### Problem
When searching, matched characters are highlighted in the list view (yellow/bold), but the content preview pane shows plain text with no highlighting.

### Current Behavior
- `highlight_matches()` creates highlighted spans using fuzzy_indices
- Only called from `render_list()` for the list item previews
- `render_preview()` renders plain text

### Solution
Introduce a unified match/highlight pipeline:
- Produce `Vec<SpanRange>` for a given line (byte ranges to highlight)
- Support both Fuzzy and Regex modes
- Cache spans for `(entry_id, query_hash, visible_line_range, mode)`
- Prefer computing spans in the background worker to avoid render-thread work

### Implementation

**Add span-based highlighting infrastructure:**
```rust
/// Byte range within a line to highlight
#[derive(Clone, Debug)]
struct SpanRange {
    start: usize,
    end: usize,
}

/// Cached highlight spans for an entry
struct HighlightCache {
    entry_id: EntryId,
    query_hash: u64,
    mode: SearchMode,
    spans_by_line: HashMap<usize, Vec<SpanRange>>,
}
```

**Modify `render_preview()` for Focus::Preview mode:**

Find the section that builds visible lines and change from:
```rust
let lines: Vec<Line> = visible_lines
    .iter()
    .map(|line| Line::raw(line.clone()))
    .collect();
```

To:
```rust
// Render uses precomputed spans when available; falls back gracefully.
// Heavy matching work must not be done on the render thread for large histories.
let lines: Vec<Line> = if !self.search_query.is_empty() {
    visible_lines
        .iter()
        .enumerate()
        .map(|(line_idx, line)| {
            let spans = self.get_cached_spans_or_compute(line_idx, line);
            self.apply_highlight_spans(line, &spans)
        })
        .collect()
} else {
    visible_lines
        .iter()
        .map(|line| Line::raw(*line))
        .collect()
};
```

**Modify `render_preview()` for Focus::List mode:**

Find where the truncated preview lines are built and change from plain text to highlighted:
```rust
// Create highlighted lines if search is active
let preview_lines: Vec<Line> = if !self.search_query.is_empty() {
    lines.iter()
        .take(max_lines)
        .map(|line| Line::from(self.highlight_matches(line)))
        .collect()
} else {
    lines.iter()
        .take(max_lines)
        .map(|line| Line::raw(*line))
        .collect()
};

let preview = Paragraph::new(preview_lines)
    // ... rest unchanged
```

### Edge Cases
- Empty search query: skip highlighting, render plain text
- No matches on visible lines: `highlight_matches` returns plain span (already handled)
- Performance: highlighting runs per-render; acceptable for typical preview sizes

### TDD Test Cases
Add to `mod tests` in picker.rs:
- `test_highlight_matches_returns_styled_spans`: verify matched chars get yellow/bold style
- `test_highlight_matches_empty_query_returns_plain`: no styling when query is empty
- `test_highlight_matches_no_match_returns_plain`: unmatched text returns single plain span
- `test_match_spans_byte_ranges_valid_utf8_boundaries`: spans never split UTF-8 characters
- `test_preview_highlighting_only_computes_visible_lines`: off-screen lines not highlighted
- `test_regex_highlight_spans_multiple_matches_per_line`: regex finds all matches on a line

### Manual Testing
1. Search for a term that appears in content
2. Select matching item, verify matches highlighted in preview pane (yellow + bold)
3. Tab into scrollable preview, verify highlighting persists
4. Scroll up/down, verify highlighting appears on all matching lines
5. Clear search, verify highlighting disappears

---

## Feature 4: Cursor Stability on Unpin (with General Selection Policy)

### Problem
When unpinning an item, the cursor follows the item to its new position (after pinned items). User wants the cursor to stay at the same list position.

### Desired Behavior
- **Pin**: Cursor follows item to top of list (user sees newly pinned item)
- **Unpin**: Cursor stays in place (item moves away from cursor)

### Current Behavior
`sort_entries_by_pin()` always restores selection by entry ID.

### Solution
Introduce a general selection-restore policy and use it consistently across list mutations. This same pattern applies to delete, time filters, regex mode, and multi-select.

### Implementation

**Define selection restore policy:**
```rust
enum SelectionRestore {
    FollowId,     // keep the same entry selected if possible
    KeepPosition, // keep the same visual row index if possible
    Nearest,      // pick nearest valid row if selected entry disappears
}
```

**Modify `sort_entries_by_pin()` signature and logic:**

```rust
/// Sort entries with pinned items first.
///
/// # Arguments
/// * `restore` - How to restore selection after the sort
fn sort_entries_by_pin(&mut self, restore: SelectionRestore) {
    // Save current state
    let selected_pos = self.selected.selected();
    let selected_id = matches!(restore, SelectionRestore::FollowId)
        .then(|| self.selected_entry().map(|e| e.id.clone()))
        .flatten();

    // Sort: pinned first, then by timestamp within each group
    self.entries.sort_by(|a, b| {
        match (a.pinned, b.pinned) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => b.timestamp.cmp(&a.timestamp),
        }
    });

    self.update_filter();

    // Restore selection based on policy
    if let Some(id) = selected_id {
        // FollowId: find by ID
        for (i, &idx) in self.filtered.iter().enumerate() {
            if self.entries[idx].id == id {
                self.selected.select(Some(i));
                break;
            }
        }
    } else if let Some(pos) = selected_pos {
        // KeepPosition or Nearest: clamp position to valid range
        let new_pos = pos.min(self.filtered.len().saturating_sub(1));
        self.selected.select(Some(new_pos));
    }

    self.update_scroll_state();
    self.load_preview();
}
```

**Modify `toggle_pin_selected()`:**

Change the call to use the policy:
```rust
// is_pinned is the NEW state after toggle
// Pin (true): follow item to top
// Unpin (false): keep cursor position
self.sort_entries_by_pin(if is_pinned {
    SelectionRestore::FollowId
} else {
    SelectionRestore::KeepPosition
});
```

### Edge Cases
- Unpin when cursor at last position: clamp to new last valid index
- Empty filtered list after sort: selection becomes None (handled by `update_filter`)
- Unpin last pinned item: cursor stays, now points to first unpinned item

### TDD Test Cases (selection invariants)
Add to `mod tests` in picker.rs:
- `test_unpin_cursor_stays_at_position`: unpin item at pos 2, cursor should stay at pos 2
- `test_pin_cursor_follows_item`: pin item, cursor moves to new position in pinned section
- `test_unpin_cursor_clamps_to_valid_range`: cursor at end, unpin causes list reorder, cursor clamps
- `test_selection_never_out_of_bounds_after_mutation`
- `test_restore_nearest_when_selected_deleted`

### Manual Testing
1. Have 3 pinned items at top, cursor on item 2
2. Unpin item 2 → cursor should stay at position 2 (now showing item 3)
3. Move cursor to an unpinned item at position 5
4. Pin it → cursor should follow to top (position 0-2 range)
5. Verify preview updates correctly after both operations

---

## Summary

| Feature | Location | Key Change |
|---------|----------|------------|
| 1. Stricter search | `filter_entries()` | Add `min_score` threshold + incremental deep search |
| 2. Ctrl+d/u scroll | `handle_preview_mode()` | Add key handlers |
| 3. Content highlighting | `render_preview()` | Call `highlight_matches()` |
| 4. Cursor on unpin | `sort_entries_by_pin()` | `SelectionRestore` policy |

## Verification Checklist

### Before Implementation
- [ ] `cargo test` passes (establish baseline)
- [ ] Read and understand existing test patterns in picker.rs tests module

### Per Feature (TDD Cycle)
- [ ] Write failing tests first (RED)
- [ ] Implement minimal code to pass (GREEN)
- [ ] Refactor if needed (REFACTOR)

### After All Features
- [ ] `cargo fmt --check` passes
- [ ] `cargo test` passes (all new + existing)
- [ ] `cargo clippy` has no warnings
- [ ] Manual testing of all 4 features
- [ ] Help text updated for new keybindings

---

# PHASE 2: UI Polish & Visual Refinement

## Feature 5: Premium Color Scheme Redesign

### Problem
Current colors are functional but lack visual cohesion. The UI feels utilitarian rather than polished. Colors are scattered across the codebase without a unified palette.

### Current Color Usage (picker.rs)
- Yellow: Pin indicator (★), search highlights, warnings, preview mode border
- Cyan: Size text `[1.5KB]`, preview border
- Blue: Selected item background
- DarkGray: Timestamps, help text
- Magenta: `[content]` deep match indicator
- Green: Success status messages
- White: Default text

### Desired Aesthetic: Stripe/Linear-inspired
Premium feel with subtle, intentional colors. Not flashy—refined.

### Solution: Unified Theme (Palette + Semantic Styles)

**Add new module `src/theme.rs`:**
```rust
//! ClipStack theme definitions - Premium/Polished aesthetic
//!
//! Design principles:
//! - Muted base colors, vibrant accents used sparingly
//! - High contrast for readability
//! - Visual hierarchy through color intensity

use ratatui::style::{Color, Style, Modifier};

/// Terminal capability modes for fallback rendering
pub enum ThemeMode {
    TrueColor,  // Full RGB support
    Ansi256,    // 256-color terminals
    Ansi16,     // Basic 16-color terminals
}

/// Semantic styles for consistent UI rendering
pub struct Theme {
    pub text_primary: Style,
    pub text_muted: Style,
    pub selection_bg: Style,
    pub badge_normal: Style,
    pub badge_active: Style,
    pub accent_primary: Style,
    pub accent_warning: Style,
    pub accent_success: Style,
    pub accent_metadata: Style,
}

/// Base colors - neutral tones for structure
pub mod base {
    use super::*;

    /// Background for selected items - subtle blue tint
    pub const SELECTION_BG: Color = Color::Rgb(30, 41, 59);      // slate-800

    /// Subtle borders
    pub const BORDER: Color = Color::Rgb(71, 85, 105);           // slate-600

    /// Focused/active borders
    pub const BORDER_FOCUS: Color = Color::Rgb(99, 102, 241);    // indigo-500

    /// Muted secondary text
    pub const TEXT_MUTED: Color = Color::Rgb(148, 163, 184);     // slate-400

    /// Primary text
    pub const TEXT_PRIMARY: Color = Color::Rgb(226, 232, 240);   // slate-200
}

/// Accent colors - used sparingly for emphasis
pub mod accent {
    use super::*;

    /// Primary accent - for interactive elements, highlights
    pub const PRIMARY: Color = Color::Rgb(99, 102, 241);         // indigo-500

    /// Success - confirmations, completed actions
    pub const SUCCESS: Color = Color::Rgb(34, 197, 94);          // green-500

    /// Warning - deletions, caution states
    pub const WARNING: Color = Color::Rgb(251, 191, 36);         // amber-400

    /// Pin indicator - warm gold
    pub const PINNED: Color = Color::Rgb(251, 191, 36);          // amber-400

    /// Search match highlight
    pub const MATCH: Color = Color::Rgb(251, 191, 36);           // amber-400

    /// Deep match (content) indicator
    pub const DEEP_MATCH: Color = Color::Rgb(168, 85, 247);      // purple-500

    /// Size/metadata
    pub const METADATA: Color = Color::Rgb(34, 211, 238);        // cyan-400
}

/// Content type colors for syntax-aware display
pub mod content {
    use super::*;

    /// URLs and links
    pub const URL: Color = Color::Rgb(96, 165, 250);             // blue-400

    /// Code snippets
    pub const CODE: Color = Color::Rgb(52, 211, 153);            // emerald-400

    /// Plain text (default)
    pub const TEXT: Color = Color::Rgb(226, 232, 240);           // slate-200
}
```

### Theme Fallback Rules (must be explicit)
- Provide deterministic mappings from RGB to nearest ANSI256/ANSI16 for:
  - selection background
  - focus border/accent
  - muted text
  - warning/success
- Allow user override via config/env:
  - `CLIPSTACK_THEME_MODE=truecolor|ansi256|ansi16`

```rust
impl ThemeMode {
    pub fn from_env() -> Self {
        match std::env::var("CLIPSTACK_THEME_MODE").as_deref() {
            Ok("ansi16") => ThemeMode::Ansi16,
            Ok("ansi256") => ThemeMode::Ansi256,
            Ok("truecolor") => ThemeMode::TrueColor,
            _ => Self::detect(),
        }
    }

    fn detect() -> Self {
        // Check $COLORTERM, $TERM for capability detection
        if std::env::var("COLORTERM").map(|v| v.contains("truecolor")).unwrap_or(false) {
            ThemeMode::TrueColor
        } else {
            ThemeMode::Ansi256
        }
    }

    pub fn map_color(&self, rgb: Color) -> Color {
        match self {
            ThemeMode::TrueColor => rgb,
            ThemeMode::Ansi256 => self.rgb_to_ansi256(rgb),
            ThemeMode::Ansi16 => self.rgb_to_ansi16(rgb),
        }
    }
}
```

### Readability Invariants (tested)
These invariants must hold in all theme modes to ensure usability:
- Selection fg must not equal selection bg
- Muted text must differ from primary text
- Focus border must differ from normal border
- Match highlight must be visible against both selected and unselected backgrounds

### Implementation Changes (picker.rs)

**Replace hardcoded colors with theme constants:**

```rust
// Before
Style::default().fg(Color::Yellow)

// After
use crate::theme::{accent, base};
Style::default().fg(accent::PINNED)
```

**Key replacements:**

| Current | Theme Constant | Usage |
|---------|----------------|-------|
| `Color::Yellow` (pin) | `accent::PINNED` | ★ indicator |
| `Color::Yellow` (match) | `accent::MATCH` | Search highlights |
| `Color::Cyan` (size) | `accent::METADATA` | Size badges |
| `Color::Blue` (selection) | `base::SELECTION_BG` | Selected row |
| `Color::DarkGray` | `base::TEXT_MUTED` | Timestamps, hints |
| `Color::Magenta` | `accent::DEEP_MATCH` | [content] tag |
| `Color::Green` | `accent::SUCCESS` | Success messages |

### Visual Hierarchy Improvements

**1. Entry list row structure (refined spacing):**
```
★  5m ago  [1.2KB]  ● Preview text here...
│    │        │     │
│    │        │     └─ Content type indicator (optional)
│    │        └─ Metadata (cyan-400)
│    └─ Muted timestamp (slate-400)
└─ Pin in warm amber (amber-400)
```

**2. Selection highlight:**
- Background: Subtle slate-800 (not bright blue)
- Text: Stays readable (no color inversion)
- Border-left: 2-char accent bar `▌` in indigo-500

**3. Preview pane header:**
```
┌─ Preview ─ 1.2KB ─ 5m ago ─────────────────┐
```
Metadata inline in header, muted colors.

### TDD Test Cases (revised)
- `test_theme_has_no_unstyled_critical_paths`: selection, badges, warnings always styled
- `test_theme_fallback_modes_compile`: ThemeMode variants construct without panics
- `test_theme_readability_invariants_hold_in_all_modes`: all modes pass readability checks
- `test_theme_env_override_respected`: CLIPSTACK_THEME_MODE forces specific mode
- `test_theme_ansi16_fallback_produces_valid_colors`: no invalid color codes

### Manual Testing
1. Open picker with various entries
2. Verify color consistency across all states
3. Check readability in different terminal themes (dark/light)
4. Verify no jarring color transitions

---

## Feature 6: Minimum Terminal Size & Responsive Layout

### Problem
The picker crashes or renders poorly in small terminals. No graceful degradation.

### Current Layout
- Fixed 40/60 split for list/preview
- No minimum size checks
- Layout constraints don't adapt

### Solution

**Add minimum size constants:**
```rust
/// Minimum terminal dimensions for usable UI
const MIN_TERMINAL_WIDTH: u16 = 60;
const MIN_TERMINAL_HEIGHT: u16 = 12;

/// Breakpoints for responsive layout
const NARROW_WIDTH: u16 = 80;   // Switch to single-pane layout
const WIDE_WIDTH: u16 = 120;    // Expand preview panel
```

**Implement size check during draw (ratatui-friendly):**
```rust
fn check_terminal_size(&self, area: Rect) -> bool {
    if area.width < MIN_TERMINAL_WIDTH || area.height < MIN_TERMINAL_HEIGHT {
        return false;
    }
    true
}

// In draw():
// if !check_terminal_size(f.area()) { render_size_warning(f); return; }
```

**Responsive layout logic (revised):**
- If `width < NARROW_WIDTH`: **single-pane list**, preview via overlay (Tab)
- Else: **horizontal split** (list | preview)

```rust
enum LayoutMode {
    SinglePane,
    Split { list_pct: u16, preview_pct: u16 }
}

fn layout_mode(area: Rect) -> LayoutMode {
    match area.width {
        w if w < NARROW_WIDTH => LayoutMode::SinglePane,
        w if w < WIDE_WIDTH => LayoutMode::Split { list_pct: 40, preview_pct: 60 },
        _ => LayoutMode::Split { list_pct: 35, preview_pct: 65 },
    }
}
```

**Resize handling:**
- Recompute layout on every draw tick and on terminal resize events
- Keep scroll/selection clamped after resize (prevents panics)

**Small terminal fallback screen:**
```rust
fn render_size_warning(&self, frame: &mut Frame) {
    let msg = vec![
        Line::from("Terminal too small"),
        Line::from(""),
        Line::from(format!("Need: {}x{}", MIN_TERMINAL_WIDTH, MIN_TERMINAL_HEIGHT)),
        Line::from(format!("Have: {}x{}", frame.area().width, frame.area().height)),
    ];
    // Center and render
}
```

### Narrow Mode Behavior (< 80 cols)
- Hide preview pane entirely
- Full-width list view
- Press `Tab` to see preview in overlay; `Esc/Tab` closes overlay
- Help text: "Tab: Preview  Enter: Copy"

### TDD Test Cases
- `test_layout_wide_terminal`: Verify 35/65 split at 120+ cols
- `test_layout_standard_terminal`: Verify 40/60 split at 80-119 cols
- `test_layout_narrow_terminal`: Verify single-pane at <80 cols
- `test_minimum_size_check`: Verify warning shown at <60x12
- `test_selection_clamped_after_resize`: No panics on dramatic resize
- `test_narrow_mode_tab_opens_preview_overlay`: Tab in narrow mode shows preview overlay
- `test_overlay_closes_on_esc_and_restores_focus`: Esc closes overlay, focus returns to list

### Manual Testing
1. Resize terminal to various sizes
2. Verify layout adapts smoothly
3. Test minimum size warning appears correctly
4. Verify narrow mode is usable with Tab for preview overlay
5. Verify overlay closes on Esc and Tab in narrow mode

---

## Feature 7: Visual Affordances & Polish

### Problem
UI lacks visual cues that indicate interactivity and state.

### Improvements

**1. Mode indicator with visual distinction:**
```rust
// Current
"[NORMAL]" / "[SEARCH]"

// Improved - colored badges
fn render_mode_badge(&self) -> Span {
    match self.mode {
        Mode::Normal => Span::styled(
            " NORMAL ",
            Style::default()
                .bg(base::BORDER)
                .fg(base::TEXT_PRIMARY)
        ),
        Mode::Search => Span::styled(
            " SEARCH ",
            Style::default()
                .bg(accent::PRIMARY)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        ),
    }
}
```

**2. Selection indicator (left accent bar):**
```rust
// Add visual "cursor" for selected item
let prefix = if is_selected {
    Span::styled("▌ ", Style::default().fg(accent::PRIMARY))
} else {
    Span::raw("  ")
};
```

**3. Scrollbar styling:**
```rust
Scrollbar::default()
    .track_symbol(Some("│"))
    .thumb_symbol("█")
    .begin_symbol(None)
    .end_symbol(None)
    .track_style(Style::default().fg(base::BORDER))
    .thumb_style(Style::default().fg(accent::PRIMARY))
```

**4. Entry count badge in title:**
```
History ─────────────────── 42/100 ─
                              │
                              └─ Pill-style count
```

**5. Search box with icon:**
```
┌─ 🔍 Search ──────────────────────┐
│ your query here                  │
└──────────────────────────────────┘
```
(Use `>` if emoji not supported)

**6. Loading state for preview:**
```rust
fn render_loading_preview(&self, frame: &mut Frame, area: Rect) {
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let idx = (Instant::now().elapsed().as_millis() / 100) % 10;
    // Render spinner with "Loading..."
}
```

**7. Keyboard hint styling:**
```rust
// Current: plain text
"j/k:Nav  /:Search  Tab:Preview"

// Improved: highlighted keys
fn render_hint(key: &str, action: &str) -> Vec<Span> {
    vec![
        Span::styled(key, Style::default().fg(accent::METADATA).add_modifier(Modifier::BOLD)),
        Span::styled(format!(":{} ", action), Style::default().fg(base::TEXT_MUTED)),
    ]
}
```

### TDD Test Cases
- `test_mode_badge_colors`: Verify correct colors per mode
- `test_selection_indicator_shows`: Verify ▌ appears on selected row

---

# PHASE 3: Power-User Productivity

## Feature 8: Multi-Select Mode

### Problem
Users can only operate on one entry at a time. Bulk delete/copy requires repetitive actions.

### Solution

**Add multi-select state (using stable IDs, not indices):**
```rust
struct Picker {
    // ... existing fields
    multi_select: HashSet<EntryId>,  // Stable selection across filtering/sorting
    multi_mode: bool,                 // Whether multi-select is active
    multi_anchor: Option<EntryId>,    // Range selection anchor (for V)
}
```

**Why IDs not indices:** Index-based selection (`HashSet<usize>`) will silently break when:
- filters change
- sorting changes
- pin/unpin reorders
- time filter toggles

Storing `entry_id` is the only stable representation across view changes.

**Keybindings:**
- `Space` - Toggle selection on current item
- `v` - Enter visual/multi-select mode
- `V` - Select range (from last selected to current)
- `Ctrl+a` - Select all (filtered)
- `Escape` - Clear selection, exit multi-mode
- `d` (in multi-mode) - Delete all selected
- `y` (in multi-mode) - Yank/merge selected to clipboard

**Visual indicators:**
```
[✓] ★  5m ago  [1.2KB]  Selected item
[ ]    3m ago  [500B]   Unselected item
[✓]    1m ago  [2.3KB]  Another selected
```

**Status line in multi-mode:**
```
[VISUAL] 3 selected ─ Space:Toggle  d:Delete  y:Yank  Esc:Cancel
```

**Merge/yank behavior (stable ordering, async):**
- UI issues `Effect::PrepareClipboard(selection_ids, order)`
- Worker loads content (bounded, cancellable), streams progress
- UI shows loading state ("Preparing clipboard..."); completion triggers clipboard write and success banner
- Cancellable via Esc during loading

```rust
// Async yank: worker loads contents, UI shows progress
fn request_yank_selected(&mut self) {
    let ids: Vec<EntryId> = self.filtered.iter()
        .map(|&entry_idx| &self.entries[entry_idx])
        .filter(|e| self.multi_select.contains(&e.id))
        .map(|e| e.id.clone())
        .collect();

    // Send to worker, show "Preparing clipboard..." banner
    self.worker_tx.send(WorkerRequest::PrepareClipboard {
        ids,
        request_id: self.next_request_id(),
        generation: self.yank_generation,
    });
    self.show_banner("Preparing clipboard...", BannerType::Loading);
}
```

**Deletion semantics (safety):**
- `d` in multi-mode opens a confirm overlay: "Delete N items? (y/n)"
- Deletes are soft by default: entries moved to a tombstone list for 30s (undo window) or until exit
- A background GC prunes tombstones based on retention settings
- `u` triggers undo if within the undo window

```rust
struct Tombstone {
    entry: Entry,
    deleted_at: Instant,
    undo_until: Instant,  // 30s default
}
```

### TDD Test Cases
- `test_multi_select_toggle`: Space toggles selection
- `test_multi_select_range`: V selects range
- `test_multi_delete`: d removes all selected
- `test_multi_delete_requires_confirm`: d shows confirm overlay first
- `test_multi_delete_undo_restores`: u within window restores deleted entries
- `test_multi_yank_is_cancellable_and_non_blocking`: Esc cancels yank, UI stays responsive
- `test_multi_yank_merges`: y combines entries with separator
- `test_multi_select_survives_resort`: pin/unpin or time filter doesn't corrupt selection

### Manual Testing
1. Select multiple items using Space
2. Verify selection state persists across scroll
3. Press V to enter range mode
4. Select range from first to last
5. Verify all items in range selected
6. Press d to delete selected
7. Verify only selected items deleted

---

## Feature 9: Regex Search Mode

### Problem
Fuzzy search is great for quick lookups but power users need exact pattern matching.

### Solution

**Add search mode toggle:**
```rust
enum SearchMode {
    Fuzzy,  // Default SkimMatcherV2
    Regex,  // regex crate
}
```

**Keybindings:**
- `/` - Enter fuzzy search (default)
- `:` - Enter regex search mode (primary; terminal-safe)
- `Ctrl+/` - Optional secondary binding

Note: `:` is the primary binding because `Ctrl+/` is inconsistent across terminals/OS keymaps.

**Visual distinction:**
```
┌─ 🔍 Search (fuzzy) ────────────────┐
│ query                               │

┌─ /regex/ Search ───────────────────┐
│ pattern.*here                       │
```

**Regex filter implementation (with caching):**
```rust
fn filter_entries_regex(&self, pattern: &str) -> Vec<FilteredEntry> {
    // Cache compiled regex per pattern to avoid recompile on each tick.
    let re = match self.regex_cache.get_or_compile(pattern) {
        Ok(r) => r,
        Err(_) => return vec![],  // Invalid regex, show nothing
    };

    self.entries
        .iter()
        .enumerate()
        .filter(|(_, e)| re.is_match(&e.preview))
        .map(|(idx, _)| FilteredEntry {
            index: idx,
            score: 100,  // Regex matches are all equal relevance
            match_location: MatchLocation::Preview,
        })
        .collect()
}
```

**Error state for invalid regex:**
```
┌─ /regex/ Search ─ ⚠ Invalid pattern ─┐
│ [unclosed                             │
```

### Dependencies
Add to `Cargo.toml`:
```toml
regex = "1"
```

**Edge Cases:**
- Empty pattern: matches everything (handled in `update_filter`)
- Invalid regex: show error state with `[Invalid pattern]` tag
- Empty preview: no matches shown (handled in `filter_entries_regex`)

### TDD Test Cases
- `test_regex_search_basic`: `/pattern.*here/` matches "pattern here"
- `test_regex_search_pattern`: `/\d{3}-\d{4}/` matches phone numbers
- `test_regex_invalid_shows_error`: Unclosed bracket shows error state
- `test_regex_case_insensitive`: `/(?i)pattern.*here/` matches "pattern here"
- `test_regex_compilation_cached`: repeated renders don't recompile

### Manual Testing
1. Press `:` to enter regex mode
2. Type `/pattern.*here/` and search
3. Verify only matching items show
4. Test invalid regex shows error state
5. Test case-insensitive search works

---

## Feature 10: Time-Based Filtering

### Problem
No way to filter by recency. Users often want "what I copied in the last hour."

### Solution

**Quick filter keybindings:**
```
1 - Last hour
2 - Last 24 hours
3 - Last 7 days
4 - All time (default)
0 - Clear time filter
```

**Filter implementation:**
```rust
enum TimeFilter {
    All,
    LastHour,
    Last24Hours,
    Last7Days,
}

fn apply_time_filter(&self, entries: &[FilteredEntry]) -> Vec<FilteredEntry> {
    let now = chrono::Utc::now().timestamp_millis();
    let cutoff = match self.time_filter {
        TimeFilter::All => 0,
        TimeFilter::LastHour => now - 3_600_000,
        TimeFilter::Last24Hours => now - 86_400_000,
        TimeFilter::Last7Days => now - 604_800_000,
    };

    entries
        .iter()
        .filter(|e| self.entries[e.index].timestamp >= cutoff)
        .cloned()
        .collect()
}
```

### Pipeline Ordering (revised)
1) Build candidate set (metadata-only): apply time filter first
2) Apply query filter (preview fuzzy/regex)
3) Trigger deep content search only for candidates not matched in preview

This ordering is critical for performance: applying time filtering after fuzzy work wastes CPU and I/O.

### Pinned Behavior (explicit)
- **Option A (default)**: pinned entries are still subject to time filter
- **Option B (power-user toggle)**: pinned entries always visible regardless of time filter

**Visual indicator:**
```
History (42/100) ─ Last hour ─────────
                   ^^^^^^^^^^
                   Time filter badge
```

### TDD Test Cases
- `test_time_filter_last_hour`: Only recent entries shown
- `test_time_filter_clears`: 0 key resets to all
- `test_time_filter_combined_with_search`: Both filters work together
- `test_time_filter_applies_before_query_work`: no content loads when outside cutoff
- `test_pinned_time_filter_policy_respected`

### Manual Testing
1. Apply time filter "last hour"
2. Verify only recent entries show
3. Clear filter, verify all entries restored
4. Search with time filter active, verify both filters work

---

## Feature 11: Keyboard Shortcut Quick Reference (?)

### Problem
Users must remember keybindings or read external docs.

### Solution

**`?` key opens help overlay:**
```
╭──────────────────────────────────────────────╮
│              ClipStack Help                  │
├──────────────────────────────────────────────┤
│  NAVIGATION                                  │
│    j/k, ↑/↓      Move selection              │
│    g/G           Jump to top/bottom          │
│    Ctrl+d/u      Half-page scroll            │
│    /             Start search                │
│    Esc           Exit search / close help    │
│                                              │
│  ACTIONS                                     │
│    Enter         Copy to clipboard & exit    │
│    p             Toggle pin                  │
│    d             Delete entry                │
│    u             Undo delete (5s)            │
│    Tab           Toggle preview focus        │
│                                              │
│  MULTI-SELECT (v to enter)                   │
│    Space         Toggle selection            │
│    V             Select range                │
│    d             Delete selected             │
│    y             Yank/merge selected         │
│                                              │
│  FILTERS                                     │
│    1-4           Time filter (1h/24h/7d/all) │
│    :             Regex search mode           │
│                                              │
│           Press ? or Esc to close            │
╰──────────────────────────────────────────────╯
```

**Implementation (revised - action-driven input system):**
- Introduce `Action` enum (domain-level intents), and `Effect` for side effects:
  - `Action::MoveSelection(delta)`, `Action::TogglePin`, `Action::EnterSearch(mode)`, etc.
  - `Effect::RequestPreviewLoad(entry_id)`, `Effect::CopyToClipboard(text)`, etc.
- A single `dispatch(KeyEvent, Context) -> Action` table prevents drift.
- Status line hints and help overlay render from the same binding registry.

```rust
/// Domain-level user intents (what the user wants to do)
enum Action {
    MoveSelection(i32),     // +1 = down, -1 = up
    JumpToTop,
    JumpToBottom,
    HalfPageDown,
    HalfPageUp,
    TogglePin,
    DeleteEntry,
    EnterSearch(SearchMode),
    ExitSearch,
    TogglePreviewFocus,
    CopyAndExit,
    ToggleMultiSelect,
    SelectRange,
    YankSelected,
    DeleteSelected,
    OpenHelp,
    CloseOverlay,
    Quit,
    Noop,
}

/// Side effects produced by actions
enum Effect {
    RequestPreviewLoad(EntryId),
    CopyToClipboard(String),
    TriggerDeepSearch(String),
    ShowBanner(String, Duration),
}

enum Overlay {
    None,
    Help,
    PreviewNarrow,  // For narrow terminal mode
}

/// Binding definition for both dispatch and documentation
struct Binding {
    context: Context,
    keys: &'static str,
    action: Action,
    description: &'static str,
}

/// Context determines which bindings are active
enum Context {
    Normal,
    SearchFuzzy,
    SearchRegex,
    Preview,
    MultiSelect,
    Overlay,
}

static BINDINGS: &[Binding] = &[
    // Navigation (Normal mode)
    Binding { context: Context::Normal, keys: "j/↓", action: Action::MoveSelection(1), description: "Move down" },
    Binding { context: Context::Normal, keys: "k/↑", action: Action::MoveSelection(-1), description: "Move up" },
    Binding { context: Context::Normal, keys: "g", action: Action::JumpToTop, description: "Jump to top" },
    Binding { context: Context::Normal, keys: "G", action: Action::JumpToBottom, description: "Jump to bottom" },
    Binding { context: Context::Normal, keys: "Ctrl+d", action: Action::HalfPageDown, description: "Half page down" },
    Binding { context: Context::Normal, keys: "Ctrl+u", action: Action::HalfPageUp, description: "Half page up" },
    // Actions
    Binding { context: Context::Normal, keys: "Enter", action: Action::CopyAndExit, description: "Copy & exit" },
    Binding { context: Context::Normal, keys: "p", action: Action::TogglePin, description: "Toggle pin" },
    Binding { context: Context::Normal, keys: "d", action: Action::DeleteEntry, description: "Delete entry" },
    Binding { context: Context::Normal, keys: "/", action: Action::EnterSearch(SearchMode::Fuzzy), description: "Fuzzy search" },
    Binding { context: Context::Normal, keys: ":", action: Action::EnterSearch(SearchMode::Regex), description: "Regex search" },
    Binding { context: Context::Normal, keys: "Tab", action: Action::TogglePreviewFocus, description: "Toggle preview" },
    Binding { context: Context::Normal, keys: "v", action: Action::ToggleMultiSelect, description: "Multi-select" },
    Binding { context: Context::Normal, keys: "?", action: Action::OpenHelp, description: "Show help" },
    Binding { context: Context::Normal, keys: "q/Esc", action: Action::Quit, description: "Quit" },
    // ... additional bindings for other contexts
];

fn dispatch(key: KeyEvent, context: Context) -> Action {
    // Match key against bindings for current context
    BINDINGS.iter()
        .find(|b| b.context == context && key_matches(&key, b.keys))
        .map(|b| b.action.clone())
        .unwrap_or(Action::Noop)
}

fn render_help_overlay(&self, frame: &mut Frame) {
    let area = centered_rect(60, 80, frame.area());
    // Generate help content directly from BINDINGS table
    // Groups by context, formats keys and descriptions
}

fn render_status_hints(&self) -> Vec<Span> {
    // Generate status line hints from BINDINGS for current context
    // Same source of truth as help overlay
}
```

### TDD Additions (input consistency)
- `test_no_duplicate_bindings_in_same_context`: no key conflicts within a context
- `test_help_is_generated_from_dispatch_table`: help content matches actual bindings

### TDD Test Cases
- `test_help_opens_on_question`: ? key sets overlay to Help
- `test_help_closes_on_escape`: Esc returns to normal
- `test_help_matches_status_line_bindings`: prevents drift between help and status line

### Manual Testing
1. Press `?` to open help
2. Verify all keybindings listed correctly
3. Press Esc to close help
4. Verify help closes and returns to normal mode

---

# PHASE 4: Code-Aware Features

## Feature 12: Syntax Highlighting for Code Snippets

### Problem
Code snippets in preview look like plain text. No syntax highlighting.

### Solution
**Gate behind feature flag** (keeps core lightweight; power users opt-in):

Add to `Cargo.toml`:
```toml
[features]
clipstack-highlight = []

[dependencies]
syntect = { version = "5", default-features = false, features = ["default-syntaxes", "default-themes", "parsing"], optional = true }
```

**Add language detection:**
```rust
/// Detect probable language from content
fn detect_language(content: &str) -> Option<&'static str> {
    let first_line = content.lines().next().unwrap_or("");

    // Shebang detection
    if first_line.starts_with("#!") {
        if first_line.contains("python") { return Some("python"); }
        if first_line.contains("bash") || first_line.contains("sh") { return Some("bash"); }
        if first_line.contains("node") { return Some("javascript"); }
    }

    // Heuristic detection
    if content.contains("fn ") && content.contains("->") { return Some("rust"); }
    if content.contains("func ") && content.contains("package ") { return Some("go"); }
    if content.contains("def ") && content.contains(":") { return Some("python"); }
    if content.contains("function") && (content.contains("const ") || content.contains("let ")) {
        return Some("javascript");
    }
    if content.contains("<?php") { return Some("php"); }
    if content.contains("import ") && content.contains("from ") { return Some("python"); }

    None
}
```

**Highlighting implementation (gated):**
```rust
#[cfg(feature = "clipstack-highlight")]
mod highlighting {
    use syntect::easy::HighlightLines;
    use syntect::highlighting::{ThemeSet, Style as SynStyle};
    use syntect::parsing::SyntaxSet;

    lazy_static! {
        static ref SYNTAX_SET: SyntaxSet = SyntaxSet::load_defaults_newlines();
        static ref THEME_SET: ThemeSet = ThemeSet::load_defaults();
    }

    pub fn highlight_code(content: &str, lang: &str) -> Vec<Line<'static>> {
        let syntax = SYNTAX_SET
            .find_syntax_by_token(lang)
            .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());

        let theme = &THEME_SET.themes["base16-ocean.dark"];
        let mut highlighter = HighlightLines::new(syntax, theme);

        content
            .lines()
            .map(|line| {
                let ranges = highlighter.highlight_line(line, &SYNTAX_SET).unwrap();
                Line::from(
                    ranges
                        .into_iter()
                        .map(|(style, text)| {
                            Span::styled(
                                text.to_string(),
                                syntect_to_ratatui_style(style),
                            )
                        })
                        .collect::<Vec<_>>()
                )
            })
            .collect()
    }

    fn syntect_to_ratatui_style(style: SynStyle) -> Style {
        // Map syntect colors to Theme semantics where possible
        Style::default().fg(Color::Rgb(
            style.foreground.r,
            style.foreground.g,
            style.foreground.b,
        ))
    }
}
```

**Content type indicator in list:**
```
★  5m ago  [1.2KB]  🦀  fn main() { println!...
                    ^^
                    Language icon/indicator
```

Icons (or fallback text):
- 🦀 / `rs` - Rust
- 🐍 / `py` - Python
- `js` - JavaScript
- `go` - Go
- `sh` - Shell/Bash
- `📄` - Plain text

### Performance Considerations
- Cache highlighted output per entry ID
- Only highlight visible lines (lazy)
- Disable for entries > 50KB
- Disable for extremely wide lines

### TDD Test Cases
- `test_detect_rust`: Content with `fn` and `->` detects as Rust
- `test_detect_python_shebang`: `#!/usr/bin/env python` detects Python
- `test_highlight_produces_spans`: Highlighted content has colored spans
- `test_highlight_fallback_plain`: Unknown language renders as plain
- `test_highlight_feature_flag_off_renders_plain`: no dependency required when disabled
- `test_highlight_uses_theme_mapping`: avoids jarring colors

### Manual Testing
1. Copy a Rust function to clipboard
2. Open ClipStack, verify preview shows syntax highlighting
3. Verify list view shows language icon
4. Verify unhighlighted content fallback works

---

## Feature 13: Content Type Indicators

### Problem
All entries look the same. No visual distinction between URLs, code, and prose.

### Solution

**Content type detection (computed at ingest, not render time):**

Detection that parses JSON or scans content is expensive if repeated during rendering/filtering. Ingest-time classification makes UI fast and consistent.

```rust
#[derive(Clone, Copy, PartialEq)]
enum ContentType {
    PlainText,
    Code(Language),
    Url,
    Json,
    Markdown,
    Path,  // File paths
}

// Detect at ingest (when clip is captured) and store in entry metadata.
// UI uses stored metadata; content loads are only needed for preview.
fn detect_content_type(content: &str) -> ContentType {
    let trimmed = content.trim();

    // URL detection
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return ContentType::Url;
    }

    // File path detection
    if trimmed.starts_with("/") || trimmed.starts_with("~/") || trimmed.contains("\\") {
        if !trimmed.contains(" ") || trimmed.contains("/") {
            return ContentType::Path;
        }
    }

    // JSON detection
    if (trimmed.starts_with("{") && trimmed.ends_with("}"))
        || (trimmed.starts_with("[") && trimmed.ends_with("]"))
    {
        if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
            return ContentType::Json;
        }
    }

    // Code detection
    if let Some(lang) = detect_language(content) {
        return ContentType::Code(lang.into());
    }

    ContentType::PlainText
}
```

### Storage/Model Change (revised)
- Add `content_type` to entry metadata persisted alongside preview/timestamp
- Optionally store `language` when content_type == Code

**Visual indicators:**

| Type | Icon | Color | Example |
|------|------|-------|---------|
| URL | 🔗 | blue-400 | `https://github.com/...` |
| Code | 🦀/🐍/etc | emerald-400 | `fn main() { ... }` |
| JSON | `{}` | amber-400 | `{"key": "value"}` |
| Path | 📁 | slate-300 | `/home/user/file.txt` |
| Text | (none) | slate-200 | Plain text... |

### TDD Test Cases
- `test_detect_url`: HTTPS URLs detected correctly
- `test_detect_json`: Valid JSON detected
- `test_detect_path`: Unix paths detected
- `test_content_type_persisted_roundtrip`: ingest -> store -> load metadata

### Manual Testing
1. Copy a URL to clipboard
2. Open ClipStack, verify list shows URL icon
3. Copy JSON data, verify list shows JSON icon
4. Copy code snippet, verify list shows language icon

---

# PHASE 5: Image Support (Future)

## Feature 14: Image Clipboard Support

### Problem
ClipStack only handles text. Users copying screenshots/images have no history.

### Scope
This is a significant architectural change requiring:
1. Binary content storage
2. Wayland image clipboard support (wl-copy/wl-paste with image types)
3. Image preview rendering (sixel/kitty graphics protocol)
4. Thumbnail generation

### Research Required
- [ ] `wl-paste -t image/png` support
- [ ] Sixel protocol support in target terminals
- [ ] Kitty graphics protocol as alternative
- [ ] Image storage format (raw vs compressed)
- [ ] Thumbnail generation (image crate)

### Proposed Architecture

**Storage changes:**
```rust
enum ClipContent {
    Text(String),
    Image {
        format: ImageFormat,
        data: Vec<u8>,
        dimensions: (u32, u32),
        thumbnail: Option<Vec<u8>>,
    },
}

enum ImageFormat {
    Png,
    Jpeg,
    Bmp,
}
```

### Storage Guidance (scales safely)
- Store blobs by hash: `blobs/<sha256>`; entries reference blob hash (dedup)
- Optional compression for thumbnails; keep original bytes for fidelity
- Add retention policy:
  - max image bytes total (e.g. 250MB configurable)
  - max age for unpinned images (e.g. 14 days configurable)
  - pinned images exempt or separately capped

### Terminal Capability Detection
- Detect: kitty graphics support / sixel support / neither
- Render best available; otherwise fallback to metadata preview (dimensions, size, format)

**Daemon changes:**
```rust
// Poll for both text and image clipboard
fn check_clipboard(&self) {
    // Text clipboard
    if let Ok(text) = Clipboard::paste() { ... }

    // Image clipboard
    if let Ok(image) = Clipboard::paste_image() { ... }
}
```

**Preview rendering:**
- Sixel: Convert image to sixel escape codes
- Kitty: Use kitty graphics protocol
- Fallback: Show dimensions and file size, ASCII art thumbnail

**List display:**
```
★  5m ago  [256KB]  🖼️  Image 1920x1080 (PNG)
```

### Dependencies
```toml
image = "0.25"
```

### Implementation Phases
1. Storage and daemon support for images
2. List view with image indicators
3. Basic preview (dimensions, size, ASCII representation)
4. Sixel preview for supported terminals
5. Kitty graphics protocol support

### TDD Test Cases
- `test_image_dedup_by_hash`: identical images reuse blob
- `test_retention_prunes_unpinned_first`: predictable cleanup behavior

### Manual Testing
1. Copy a screenshot to clipboard
2. Open ClipStack, verify image appears in list
3. Select image, verify preview shows correctly
4. Verify file size and dimensions shown
5. Test thumbnail generation for large images

---

# Implementation Priority

## Must Have (Phase 0-2)
0. Architecture & performance guardrails (Phase 0)
1. ✅ Stricter search filtering
2. ✅ Ctrl+d/u scrolling in preview
3. ✅ Highlight search matches in content
4. ✅ Cursor stability on unpin
5. Premium color scheme
6. Minimum terminal size handling

## Should Have (Phase 3)
7. Visual affordances & polish
8. Multi-select mode
9. Keyboard shortcut help (?)
10. Time-based filtering

## Nice to Have (Phase 4)
11. Regex search mode
12. Syntax highlighting for code (behind feature flag)
13. Content type indicators

## Future (Phase 5)
14. Image clipboard support

---

# Selection & State Invariants

These invariants MUST be tested and enforced across all features:

- **Selection always valid or None**: selection index never exceeds `filtered.len() - 1`
- **Preview scroll always clamped**: `preview_scroll <= max_preview_scroll()`
- **Filtered list indices always refer to valid entries**: no stale indices after mutations
- **Deep search results are ignored if stale**: query change cancels pending results

### Invariant TDD Tests
- `test_selection_never_out_of_bounds_after_any_mutation`
- `test_preview_scroll_clamped_on_content_change`
- `test_filtered_indices_valid_after_delete`
- `test_stale_deep_search_results_discarded`

---

## CI Recommendations

Run on every push:
- `cargo fmt --check` - formatting
- `cargo clippy -D warnings` - lints
- `cargo test` - all tests

This project benefits disproportionately from early guardrails.

## Testing Strategy Additions (high leverage)

### UI Snapshot Tests (ratatui buffer)
Capture `ratatui::buffer::Buffer` state for key UI scenarios:
- Empty state (no history)
- Search with matches + no matches
- Preview focused + highlighted
- Help overlay
- Terminal-too-small screen
- Multi-select mode with selections visible

### Storage Upgrade Tests
- `test_storage_vN_to_vN+1_migration`: migration produces identical visible history
- `test_storage_downgrade_shows_clear_error`: future versions fail gracefully

### Corruption Tests
- `test_truncated_blob_quarantined`: truncated entry blob is quarantined, not panic
- `test_bad_checksum_quarantined`: invalid checksum leads to quarantine + non-fatal banner
- `test_corrupt_index_rebuilds`: corrupted index triggers safe rebuild

---

## Additional Recommendations (high-value tweaks)

### Config Surface Area Discipline
- Start with a single `Config` struct and explicit defaults
- Avoid "env everywhere" pattern; prefer a unified config source
- Document all config options in one place

### Clipboard Backend Abstraction
- Define a `ClipboardBackend` trait even if only Wayland is supported initially
- This makes X11/macOS support non-invasive later:
```rust
trait ClipboardBackend {
    fn paste_text(&self) -> Result<String>;
    fn copy_text(&self, text: &str) -> Result<()>;
    fn paste_image(&self) -> Result<ImageData>;
    fn copy_image(&self, data: &ImageData) -> Result<()>;
}
```

### Entry Normalization
- Normalize line endings (CRLF → LF) at ingest
- Trim trailing NULs and control characters
- Prevents weird rendering/highlighting edge cases

### Security Posture (especially for images)
- Treat decoded image operations as untrusted input
- Cap decode sizes (e.g., max 16MP, max 50MB raw)
- Never allocate unbounded buffers based on header claims
- Sandbox image decoding if possible (separate process/seccomp)

---

## Verification Checklist

### Before Implementation
- [ ] `cargo test` passes (establish baseline)
- [ ] Read and understand existing test patterns in picker.rs tests module

### Per Feature (TDD Cycle)
- [ ] Write failing tests first (RED)
- [ ] Implement minimal code to pass (GREEN)
- [ ] Refactor if needed (REFACTOR)

### After All Features
- [ ] `cargo fmt --check` passes
- [ ] `cargo test` passes (all new + existing)
- [ ] `cargo clippy` has no warnings
- [ ] Manual testing of all features
- [ ] Help text updated for new keybindings
