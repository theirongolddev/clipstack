use crate::clipboard::Clipboard;
use crate::daemon::Daemon;
use crate::storage::{ClipEntry, Storage};
use crate::util;
use anyhow::Result;
use crossterm::{
    cursor::Show,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Wrap,
    },
    Frame, Terminal,
};
use std::collections::HashSet;
use std::io::{stdout, Stdout};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Picker mode for vim-style navigation
#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Normal,  // j/k navigation, typing starts search
    Search,  // Active search input
}

/// Status message level for toast-like feedback
#[derive(Clone, Copy)]
enum StatusLevel {
    Success,
    Warning,
}

/// Where the search match was found
#[derive(Clone, Copy, PartialEq, Debug)]
enum MatchLocation {
    Preview, // Match found in preview (fast path)
    Content, // Match found in full content (lazy load)
}

/// Entry with search metadata for filtered results
#[derive(Debug)]
struct FilteredEntry {
    index: usize,                   // Index into original entries list
    score: i64,                     // Fuzzy match score (higher = better)
    match_location: MatchLocation,  // Where match was found
}

/// Focus mode for preview scrolling
#[derive(Clone, Copy, PartialEq, Default, Debug)]
enum Focus {
    #[default]
    List,    // Normal mode - navigate entry list
    Preview, // Preview mode - scroll through selected entry content
}

/// Deleted entry for undo functionality
struct DeletedEntry {
    entry: ClipEntry,
    content: String,
    was_pinned: bool, // Track pin state for restoration
    deleted_at: Instant,
}

pub struct Picker {
    storage: Storage,
    entries: Vec<ClipEntry>,
    filtered: Vec<usize>,
    filtered_entries: Vec<FilteredEntry>, // Search results with match metadata
    selected: ListState,
    scroll_state: ScrollbarState,
    search_query: String,
    preview_content: Option<String>,
    preview_id: Option<String>,
    matcher: SkimMatcherV2,
    mode: Mode,
    status_message: Option<(String, StatusLevel, Instant)>,
    last_deleted: Option<DeletedEntry>,
    pending_g: bool,             // For gg command
    focus: Focus,                // Current focus mode (List or Preview)
    preview_scroll: usize,       // Current scroll offset in preview
    preview_lines: Vec<String>,  // Cached wrapped lines of preview content
    preview_height: u16,         // Available height for preview area
}

impl Picker {
    pub fn new(storage: Storage) -> Result<Self> {
        let index = storage.load_index()?;

        let mut picker = Self {
            storage,
            entries: index.entries,
            filtered: Vec::new(),
            filtered_entries: Vec::new(),
            selected: ListState::default(),
            scroll_state: ScrollbarState::default(),
            search_query: String::new(),
            preview_content: None,
            preview_id: None,
            matcher: SkimMatcherV2::default(),
            mode: Mode::Normal,
            status_message: None,
            last_deleted: None,
            pending_g: false,
            focus: Focus::default(),
            preview_scroll: 0,
            preview_lines: Vec::new(),
            preview_height: 10, // Updated dynamically during render
        };

        picker.update_filter();
        if !picker.filtered.is_empty() {
            picker.selected.select(Some(0));
        }
        picker.update_scroll_state();

        Ok(picker)
    }

    /// Two-phase search: first search previews (fast), then full content (lazy load)
    fn filter_entries(&self, query: &str) -> Vec<FilteredEntry> {
        let mut results: Vec<FilteredEntry> = Vec::new();

        // Phase 1: Search previews (always available, fast)
        for (idx, entry) in self.entries.iter().enumerate() {
            if let Some(score) = self.matcher.fuzzy_match(&entry.preview, query) {
                results.push(FilteredEntry {
                    index: idx,
                    score,
                    match_location: MatchLocation::Preview,
                });
            }
        }

        // Phase 2: For entries not matched in preview, search full content
        let preview_matched: HashSet<usize> = results.iter().map(|r| r.index).collect();

        for (idx, entry) in self.entries.iter().enumerate() {
            if preview_matched.contains(&idx) {
                continue; // Already matched in preview
            }

            // Lazy load content only when needed
            if let Ok(content) = self.storage.load_content(&entry.id)
                && let Some(score) = self.matcher.fuzzy_match(&content, query)
            {
                results.push(FilteredEntry {
                    index: idx,
                    score,
                    match_location: MatchLocation::Content,
                });
            }
        }

        // Sort by score descending (best matches first)
        results.sort_by(|a, b| b.score.cmp(&a.score));
        results
    }

    fn update_filter(&mut self) {
        if self.search_query.is_empty() {
            // No search query - show all entries in order
            self.filtered = (0..self.entries.len()).collect();
            self.filtered_entries.clear();
        } else {
            // Run two-phase search
            self.filtered_entries = self.filter_entries(&self.search_query);

            // Extract indices for filtered list
            self.filtered = self.filtered_entries.iter().map(|e| e.index).collect();
        }

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

    fn update_scroll_state(&mut self) {
        self.scroll_state = self
            .scroll_state
            .content_length(self.filtered.len())
            .position(self.selected.selected().unwrap_or(0));
    }

    fn selected_entry(&self) -> Option<&ClipEntry> {
        self.selected
            .selected()
            .and_then(|i| self.filtered.get(i))
            .and_then(|&idx| self.entries.get(idx))
    }

    /// Toggle pin status of selected entry
    fn toggle_pin_selected(&mut self) -> Result<()> {
        if let Some(idx) = self.selected.selected().and_then(|i| self.filtered.get(i).copied()) {
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
        let selected_id = self.selected_entry().map(|e| e.id.clone());

        self.entries.sort_by(|a, b| {
            match (a.pinned, b.pinned) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => b.timestamp.cmp(&a.timestamp), // Most recent first
            }
        });

        self.update_filter();

        // Restore selection by ID
        if let Some(id) = selected_id {
            for (i, &idx) in self.filtered.iter().enumerate() {
                if self.entries[idx].id == id {
                    self.selected.select(Some(i));
                    break;
                }
            }
        }
        self.update_scroll_state();
        self.load_preview();
    }

    /// Get the match location for a filtered entry position
    /// Returns None if no search is active or position is out of bounds
    fn get_match_location(&self, filtered_pos: usize) -> Option<MatchLocation> {
        if self.search_query.is_empty() {
            return None;
        }
        self.filtered_entries.get(filtered_pos).map(|e| e.match_location)
    }

    fn load_preview(&mut self) {
        let entry_id = self.selected_entry().map(|e| e.id.clone());

        match entry_id {
            Some(id) if self.preview_id.as_ref() != Some(&id) => {
                match self.storage.load_content(&id) {
                    Ok(content) => {
                        self.preview_content = Some(content);
                        self.preview_id = Some(id);
                    }
                    Err(_) => {
                        self.preview_content = None;
                        self.preview_id = None;
                    }
                }
            }
            None => {
                self.preview_content = None;
                self.preview_id = None;
            }
            _ => {}
        }
    }

    /// Load and wrap preview content for Focus::Preview mode
    fn load_preview_content(&mut self) {
        let entry = match self.selected_entry() {
            Some(e) => e.clone(),
            None => return,
        };

        if let Ok(content) = self.storage.load_content(&entry.id) {
            // Wrap lines to preview width (typically terminal width - padding)
            let wrap_width = 80;
            self.preview_lines = content
                .lines()
                .flat_map(|line| {
                    if line.len() <= wrap_width {
                        vec![line.to_string()]
                    } else {
                        line.chars()
                            .collect::<Vec<_>>()
                            .chunks(wrap_width)
                            .map(|c| c.iter().collect::<String>())
                            .collect()
                    }
                })
                .collect();
            self.preview_scroll = 0;
        }
    }

    /// Calculate max scroll offset for preview mode
    fn max_preview_scroll(&self) -> usize {
        self.preview_lines
            .len()
            .saturating_sub(self.preview_height as usize)
    }

    /// Handle keyboard input in Focus::Preview mode
    fn handle_preview_mode(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> Result<Option<Option<String>>> {
        match key.code {
            // Line-by-line scrolling
            KeyCode::Up | KeyCode::Char('k') => {
                self.preview_scroll = self.preview_scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max_scroll = self.max_preview_scroll();
                if self.preview_scroll < max_scroll {
                    self.preview_scroll += 1;
                }
            }

            // Page scrolling
            KeyCode::PageUp => {
                self.preview_scroll = self
                    .preview_scroll
                    .saturating_sub(self.preview_height as usize);
            }
            KeyCode::PageDown => {
                let max_scroll = self.max_preview_scroll();
                let page = self.preview_height as usize;
                self.preview_scroll = (self.preview_scroll + page).min(max_scroll);
            }

            // Jump to top/bottom
            KeyCode::Home | KeyCode::Char('g') => {
                self.preview_scroll = 0;
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.preview_scroll = self.max_preview_scroll();
            }

            // Exit preview mode
            KeyCode::Tab | KeyCode::Esc | KeyCode::Char('q') => {
                self.focus = Focus::List;
                self.preview_lines.clear();
                self.preview_scroll = 0;
            }

            _ => {}
        }

        Ok(None)
    }

    fn move_selection(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            return;
        }

        let current = self.selected.selected().unwrap_or(0) as i32;
        let new_idx = (current + delta).clamp(0, self.filtered.len() as i32 - 1) as usize;

        // Clear preview cache when selection changes
        if self.selected.selected() != Some(new_idx) {
            self.preview_lines.clear();
            self.preview_scroll = 0;
        }

        self.selected.select(Some(new_idx));
        self.update_scroll_state();
        self.load_preview();
    }

    fn jump_to_start(&mut self) {
        if !self.filtered.is_empty() {
            self.selected.select(Some(0));
            self.update_scroll_state();
            self.load_preview();
        }
    }

    fn jump_to_end(&mut self) {
        if !self.filtered.is_empty() {
            self.selected.select(Some(self.filtered.len() - 1));
            self.update_scroll_state();
            self.load_preview();
        }
    }

    fn set_status(&mut self, msg: String, level: StatusLevel) {
        self.status_message = Some((msg, level, Instant::now()));
    }

    fn delete_selected(&mut self) -> Result<()> {
        if let Some(entry) = self.selected_entry().cloned() {
            // Load content for undo
            let content = self.storage.load_content(&entry.id)?;

            // Get preview for status message
            let preview: String = entry.preview.chars().take(30).collect();
            let was_pinned = entry.pinned;

            // Store for undo
            self.last_deleted = Some(DeletedEntry {
                entry: entry.clone(),
                content,
                was_pinned,
                deleted_at: Instant::now(),
            });

            // Delete from storage
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

    fn undo_delete(&mut self) -> Result<()> {
        if let Some(deleted) = self.last_deleted.take() {
            if deleted.deleted_at.elapsed() < Duration::from_secs(5) {
                // Get preview for status message
                let preview: String = deleted.entry.preview.chars().take(30).collect();

                // Restore the entry
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

    fn render(&mut self, frame: &mut Frame) {
        // Check for empty state first
        if self.entries.is_empty() {
            self.render_empty_state(frame);
            return;
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Search box
                Constraint::Min(5),    // List + Preview
                Constraint::Length(1), // Status/Help line
            ])
            .split(frame.area());

        self.render_search_box(frame, chunks[0]);

        // Split middle into list and preview
        let middle = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(chunks[1]);

        self.render_list(frame, middle[0]);
        self.render_preview(frame, middle[1]);
        self.render_status_line(frame, chunks[2]);
    }

    fn render_empty_state(&self, frame: &mut Frame) {
        let area = frame.area();
        let center = Rect {
            x: area.width / 4,
            y: area.height / 3,
            width: area.width / 2,
            height: 10,
        };

        let lines = vec![
            Line::from(Span::styled(
                "Clipboard History Empty",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("Copy some text to get started!"),
            Line::from("The daemon saves everything you copy."),
            Line::from(""),
            Line::from(Span::styled(
                "Tip: The daemon starts automatically",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "when you open this picker.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Press ESC or 'q' to exit",
                Style::default().fg(Color::Cyan),
            )),
        ];

        let widget = Paragraph::new(lines)
            .block(
                Block::default()
                    .title("Getting Started")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Blue)),
            )
            .alignment(Alignment::Center);

        frame.render_widget(widget, center);
    }

    fn render_search_box(&self, frame: &mut Frame, area: Rect) {
        let title = match self.mode {
            Mode::Search => "Search (ESC to exit search)",
            Mode::Normal => "Search (/ to search, type to filter)",
        };

        let border_color = match self.mode {
            Mode::Search => Color::Cyan,
            Mode::Normal => Color::White,
        };

        let search_block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(border_color));

        let search_text = Paragraph::new(self.search_query.as_str()).block(search_block);
        frame.render_widget(search_text, area);

        // Show cursor only in search mode
        if self.mode == Mode::Search {
            // Use character count, not byte length, for correct Unicode cursor position
            let char_count = self.search_query.chars().count() as u16;
            frame.set_cursor_position((
                area.x + 1 + char_count,
                area.y + 1,
            ));
        }
    }

    fn render_list(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .enumerate()
            .map(|(filtered_pos, &idx)| {
                let entry = &self.entries[idx];
                let time = util::format_relative_time(entry.timestamp);
                let size = util::format_size(entry.size);

                // Check if this is a content match (not preview match)
                let is_content_match = self.get_match_location(filtered_pos)
                    == Some(MatchLocation::Content);

                // Truncate preview for list display
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

                // Add [content] indicator for deep matches
                if is_content_match {
                    spans.push(Span::styled(
                        "[content] ",
                        Style::default().fg(Color::Magenta),
                    ));
                }

                spans.extend(preview_spans);

                ListItem::new(Line::from(spans))
            })
            .collect();

        // Build title with match statistics
        let title = if !self.search_query.is_empty() {
            let content_matches = self.filtered_entries.iter()
                .filter(|fe| fe.match_location == MatchLocation::Content)
                .count();
            if content_matches > 0 {
                format!(
                    "History ({}/{}) matching '{}' ({} in content)",
                    self.filtered.len(),
                    self.entries.len(),
                    self.search_query,
                    content_matches
                )
            } else {
                format!(
                    "History ({}/{}) matching '{}'",
                    self.filtered.len(),
                    self.entries.len(),
                    self.search_query
                )
            }
        } else {
            let pinned_count = self.entries.iter().filter(|e| e.pinned).count();
            if pinned_count > 0 {
                format!(
                    "History ({}/{}) - {} pinned",
                    self.filtered.len(),
                    self.entries.len(),
                    pinned_count
                )
            } else {
                format!("History ({}/{})", self.filtered.len(), self.entries.len())
            }
        };

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
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

    /// Highlight matched characters in preview text
    fn highlight_matches(&self, text: &str) -> Vec<Span<'static>> {
        // Get match indices from fuzzy matcher
        if let Some(indices) = self.matcher.fuzzy_indices(text, &self.search_query) {
            let (_, positions) = indices;
            let mut spans = Vec::new();
            let chars: Vec<char> = text.chars().collect();
            let mut last_pos = 0;

            for &pos in &positions {
                if pos > last_pos {
                    // Non-matched portion
                    let segment: String = chars[last_pos..pos].iter().collect();
                    spans.push(Span::raw(segment));
                }
                // Matched character
                let matched: String = chars[pos..=pos].iter().collect();
                spans.push(Span::styled(
                    matched,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
                last_pos = pos + 1;
            }

            // Remaining non-matched portion
            if last_pos < chars.len() {
                let segment: String = chars[last_pos..].iter().collect();
                spans.push(Span::raw(segment));
            }

            spans
        } else {
            vec![Span::raw(text.to_string())]
        }
    }

    fn render_preview(&mut self, frame: &mut Frame, area: Rect) {
        // Update preview_height for scroll calculations
        self.preview_height = area.height.saturating_sub(2); // Account for borders

        // In Focus::Preview mode, render the scrollable preview lines
        if self.focus == Focus::Preview && !self.preview_lines.is_empty() {
            let visible_height = self.preview_height as usize;
            let start = self.preview_scroll;
            let end = (start + visible_height).min(self.preview_lines.len());
            let visible_lines = &self.preview_lines[start..end];

            let preview_text = visible_lines.join("\n");

            // Build title with scroll position
            let title = if self.preview_lines.len() > visible_height {
                format!(
                    "[PREVIEW] Lines {}-{} of {} (Tab to exit)",
                    start + 1,
                    end,
                    self.preview_lines.len()
                )
            } else {
                "[PREVIEW] Tab to exit".to_string()
            };

            // Highlight border when in preview mode
            let preview = Paragraph::new(preview_text).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(Style::default().fg(Color::Yellow)),
            );

            frame.render_widget(preview, area);
            return;
        }

        // Normal preview rendering (Focus::List mode)
        let (content, metadata) = if let Some(entry) = self.selected_entry() {
            let content = self.preview_content.as_deref().unwrap_or("(loading...)");
            let time = util::format_relative_time(entry.timestamp);
            let size = util::format_size(entry.size);
            (content, format!("Preview - {} - {}", size, time))
        } else {
            ("(no selection)", "Preview".to_string())
        };

        // Count lines and handle truncation
        let lines: Vec<&str> = content.lines().collect();
        let max_lines = self.preview_height as usize;
        let truncated = lines.len() > max_lines;

        let preview_text: String = lines
            .iter()
            .take(max_lines)
            .copied()
            .collect::<Vec<_>>()
            .join("\n");

        let title = if truncated {
            format!("{} [+{} lines, Tab to scroll]", metadata, lines.len() - max_lines)
        } else {
            metadata
        };

        let preview = Paragraph::new(preview_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(preview, area);
    }

    fn render_status_line(&mut self, frame: &mut Frame, area: Rect) {
        // Check if we have a status message that hasn't expired
        let status_text = if let Some((msg, level, instant)) = &self.status_message {
            let elapsed = instant.elapsed();
            if elapsed < Duration::from_secs(3) {
                // Show undo countdown if applicable
                let display_msg = if msg.contains("undo") {
                    if let Some(deleted) = &self.last_deleted {
                        let remaining = 5_u64.saturating_sub(deleted.deleted_at.elapsed().as_secs());
                        if remaining > 0 {
                            format!("Deleted - Press 'u' to undo ({}s)", remaining)
                        } else {
                            msg.clone()
                        }
                    } else {
                        msg.clone()
                    }
                } else {
                    msg.clone()
                };

                let style = match level {
                    StatusLevel::Success => Style::default().fg(Color::Green),
                    StatusLevel::Warning => Style::default().fg(Color::Yellow),
                };
                Some((display_msg, style))
            } else {
                self.status_message = None;
                None
            }
        } else {
            None
        };

        let (text, style) = status_text.unwrap_or_else(|| {
            // Show different help based on focus mode
            if self.focus == Focus::Preview {
                (
                    "[PREVIEW] j/k:Scroll  PgUp/Dn:Page  g/G:Top/Bottom  Tab/Esc:Back  q:Quit"
                        .to_string(),
                    Style::default().fg(Color::Yellow),
                )
            } else {
                let mode_indicator = match self.mode {
                    Mode::Normal => "[NORMAL]",
                    Mode::Search => "[SEARCH]",
                };
                (
                    format!(
                        "{} j/k:Nav  /:Search  Tab:Preview  Enter:Paste  p:Pin  d:Del  u:Undo  q:Quit",
                        mode_indicator
                    ),
                    Style::default().fg(Color::DarkGray),
                )
            }
        });

        let help = Paragraph::new(text).style(style);
        frame.render_widget(help, area);
    }

    pub fn run(&mut self) -> Result<Option<String>> {
        // Setup terminal
        let mut stdout = stdout();
        stdout.execute(EnterAlternateScreen)?;
        enable_raw_mode()?;

        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        self.load_preview();

        let result = self.event_loop(&mut terminal);

        // Cleanup - always attempt cleanup even if event_loop failed
        // This ensures terminal is restored to normal state
        // Show cursor (it may have been hidden during TUI rendering)
        let _ = terminal.show_cursor();
        // Leave alternate screen through terminal's backend (same stdout handle)
        let _ = terminal.backend_mut().execute(LeaveAlternateScreen);
        // Restore cursor visibility in normal screen too
        let _ = terminal.backend_mut().execute(Show);
        // Disable raw mode last
        let _ = disable_raw_mode();

        result
    }

    fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> Result<Option<String>> {
        loop {
            terminal.draw(|f| self.render(f))?;

            if event::poll(Duration::from_millis(100))?
                && let Event::Key(key) = event::read()?
            {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                // Handle mode-specific input
                let result = match self.mode {
                    Mode::Normal => self.handle_normal_mode(key)?,
                    Mode::Search => self.handle_search_mode(key)?,
                };

                if let Some(action) = result {
                    return Ok(action);
                }
            }

            // Clear expired undo
            if let Some(deleted) = &self.last_deleted
                && deleted.deleted_at.elapsed() >= Duration::from_secs(5)
            {
                self.last_deleted = None;
            }
        }
    }

    fn handle_normal_mode(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> Result<Option<Option<String>>> {
        // Handle preview scroll navigation when in Focus::Preview mode
        if self.focus == Focus::Preview {
            return self.handle_preview_mode(key);
        }

        // Handle pending 'g' for gg command
        if self.pending_g {
            self.pending_g = false;
            if key.code == KeyCode::Char('g') {
                self.jump_to_start();
                return Ok(None);
            }
            // Not gg, ignore the pending g
        }

        match key.code {
            // Exit
            KeyCode::Esc | KeyCode::Char('q') => return Ok(Some(None)),

            // Select
            KeyCode::Enter => {
                if let Some(entry) = self.selected_entry() {
                    let content = self.storage.load_content(&entry.id)?;
                    return Ok(Some(Some(content)));
                }
            }

            // Navigation - vim style
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),

            // Page navigation
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_selection(10)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_selection(-10)
            }
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::PageUp => self.move_selection(-10),

            // Jump to end
            KeyCode::Char('G') => self.jump_to_end(),

            // Jump to start (wait for second g)
            KeyCode::Char('g') => {
                self.pending_g = true;
            }

            // Enter search mode
            KeyCode::Char('/') => {
                self.mode = Mode::Search;
            }

            // Delete selected item
            KeyCode::Char('d') => {
                self.delete_selected()?;
            }

            // Undo
            KeyCode::Char('u') => {
                self.undo_delete()?;
            }

            // Toggle pin on selected entry
            KeyCode::Char('p') => {
                self.toggle_pin_selected()?;
            }

            // Toggle focus between List and Preview
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::List => {
                        self.load_preview_content();
                        Focus::Preview
                    }
                    Focus::Preview => {
                        self.preview_lines.clear();
                        self.preview_scroll = 0;
                        Focus::List
                    }
                };
            }

            // Quick search - any other character starts search
            KeyCode::Char(c) if c.is_alphanumeric() || c == ' ' => {
                self.mode = Mode::Search;
                self.search_query.push(c);
                self.update_filter();
                self.load_preview();
            }

            _ => {}
        }

        Ok(None)
    }

    fn handle_search_mode(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> Result<Option<Option<String>>> {
        match key.code {
            // Exit search mode
            KeyCode::Esc => {
                self.mode = Mode::Normal;
            }

            // Select from search
            KeyCode::Enter => {
                if let Some(entry) = self.selected_entry() {
                    let content = self.storage.load_content(&entry.id)?;
                    return Ok(Some(Some(content)));
                }
            }

            // Navigation in search mode
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),

            // Ctrl+N/P for navigation
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_selection(1)
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_selection(-1)
            }

            // Backspace
            KeyCode::Backspace => {
                self.search_query.pop();
                self.update_filter();
                self.load_preview();

                // Exit search mode if query is empty
                if self.search_query.is_empty() {
                    self.mode = Mode::Normal;
                }
            }

            // Type characters
            KeyCode::Char(c) => {
                self.search_query.push(c);
                self.update_filter();
                self.load_preview();
            }

            _ => {}
        }

        Ok(None)
    }
}

/// Ensure daemon is running, silently spawning if needed
fn ensure_daemon_running() {
    if Daemon::is_running() {
        return;
    }

    // Silently spawn daemon
    let _ = Command::new("clipstack")
        .arg("daemon")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    // Brief wait for daemon to start
    std::thread::sleep(Duration::from_millis(200));
}

/// Run the picker and paste the selected content to clipboard
pub fn pick_and_paste(storage: Storage) -> Result<bool> {
    // Ensure daemon is running before showing picker
    ensure_daemon_running();

    let mut picker = Picker::new(storage)?;

    match picker.run() {
        Ok(Some(content)) => {
            // Content was selected
            Clipboard::copy(&content)?;
            eprintln!("Copied {} bytes to clipboard", content.len());
            Ok(true)
        }
        Ok(None) => {
            // User cancelled (ESC/q)
            Ok(false)
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Helper: Create a test storage with entries
    fn create_test_storage(entries: &[&str]) -> (TempDir, Storage) {
        let temp = TempDir::new().unwrap();
        let storage = Storage::new(temp.path().to_path_buf(), 100).unwrap();
        for content in entries {
            storage.save_entry(content).unwrap();
        }
        (temp, storage)
    }

    // ======== FilteredEntry Type Tests ========

    #[test]
    fn test_filtered_entry_creation() {
        let fe = FilteredEntry {
            index: 5,
            score: 100,
            match_location: MatchLocation::Preview,
        };
        assert_eq!(fe.index, 5);
        assert_eq!(fe.score, 100);
        assert_eq!(fe.match_location, MatchLocation::Preview);
    }

    #[test]
    fn test_match_location_equality() {
        assert_eq!(MatchLocation::Preview, MatchLocation::Preview);
        assert_eq!(MatchLocation::Content, MatchLocation::Content);
        assert_ne!(MatchLocation::Preview, MatchLocation::Content);
    }

    // ======== Search Algorithm Tests ========

    #[test]
    fn test_preview_match_found() {
        // Entry with "hello world" preview should match "hello"
        let (_temp, storage) = create_test_storage(&["hello world content here"]);
        let picker = Picker::new(storage).unwrap();

        let results = picker.filter_entries("hello");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].match_location, MatchLocation::Preview);
    }

    #[test]
    fn test_content_search_finds_non_preview_match() {
        // Create entry with content longer than preview limit (100 chars)
        // where the search term is only in the non-preview portion
        let long_content = format!(
            "{}\nTHIS_UNIQUE_KEYWORD_NOT_IN_PREVIEW",
            "x".repeat(150) // Preview is only 100 chars
        );
        let (_temp, storage) = create_test_storage(&[&long_content]);
        let picker = Picker::new(storage).unwrap();

        let results = picker.filter_entries("THIS_UNIQUE_KEYWORD");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].match_location, MatchLocation::Content);
    }

    #[test]
    fn test_search_results_sorted_by_score() {
        // Multiple matches with different relevance
        let (_temp, storage) = create_test_storage(&[
            "hello", // Short, high relevance for "hello"
            "hello world is a greeting phrase hello", // More matches
            "say hello to everyone",
        ]);
        let picker = Picker::new(storage).unwrap();

        let results = picker.filter_entries("hello");
        assert_eq!(results.len(), 3);

        // Verify sorted by score descending
        for i in 1..results.len() {
            assert!(results[i - 1].score >= results[i].score);
        }
    }

    #[test]
    fn test_empty_query_returns_all_via_update_filter() {
        let (_temp, storage) = create_test_storage(&["one", "two", "three"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.search_query = "".to_string();
        picker.update_filter();

        // All entries should be in filtered list
        assert_eq!(picker.filtered.len(), 3);
    }

    #[test]
    fn test_no_matches_returns_empty() {
        let (_temp, storage) = create_test_storage(&["apple", "banana", "cherry"]);
        let picker = Picker::new(storage).unwrap();

        let results = picker.filter_entries("xyz_nonexistent");
        assert!(results.is_empty());
    }

    #[test]
    fn test_preview_match_not_duplicated() {
        // Entry where search term appears in both preview and content
        // Should only appear once with Preview location (not searched twice)
        let (_temp, storage) = create_test_storage(&["hello world"]);
        let picker = Picker::new(storage).unwrap();

        let results = picker.filter_entries("hello");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].match_location, MatchLocation::Preview);
    }

    // ======== Search UI State Tests ========

    #[test]
    fn test_get_match_location_with_search() {
        let (_temp, storage) = create_test_storage(&["apple", "banana"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.search_query = "apple".to_string();
        picker.update_filter();

        // Should have match location for first result
        let location = picker.get_match_location(0);
        assert!(location.is_some());
        assert_eq!(location.unwrap(), MatchLocation::Preview);
    }

    #[test]
    fn test_get_match_location_without_search() {
        let (_temp, storage) = create_test_storage(&["apple", "banana"]);
        let picker = Picker::new(storage).unwrap();

        // No search query = no match location
        let location = picker.get_match_location(0);
        assert!(location.is_none());
    }

    #[test]
    fn test_selection_resets_when_filter_shrinks() {
        let (_temp, storage) = create_test_storage(&["apple", "apricot", "banana", "cherry"]);
        let mut picker = Picker::new(storage).unwrap();

        // Select last item
        picker.selected.select(Some(3));

        // Search for "ap" - only 2 results
        picker.search_query = "ap".to_string();
        picker.update_filter();

        // Selection should reset if out of bounds
        let selected = picker.selected.selected().unwrap_or(0);
        assert!(selected < picker.filtered.len());
    }

    #[test]
    fn test_fuzzy_matching_works() {
        // Test that fuzzy matching finds partial matches
        let (_temp, storage) = create_test_storage(&["hello_world_function"]);
        let picker = Picker::new(storage).unwrap();

        // "hef" should fuzzy match "hello_world_function" (h-e-llo_world_f-unction)
        let results = picker.filter_entries("hwf");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_case_insensitive_search() {
        let (_temp, storage) = create_test_storage(&["Hello World", "HELLO", "hello"]);
        let picker = Picker::new(storage).unwrap();

        let results = picker.filter_entries("hello");
        assert_eq!(results.len(), 3);
    }

    // ======== Focus Mode Tests ========

    #[test]
    fn test_focus_enum_default() {
        assert_eq!(Focus::default(), Focus::List);
    }

    #[test]
    fn test_focus_toggle_list_to_preview() {
        let (_temp, storage) = create_test_storage(&["test content"]);
        let mut picker = Picker::new(storage).unwrap();

        assert_eq!(picker.focus, Focus::List);
        picker.focus = Focus::Preview;
        assert_eq!(picker.focus, Focus::Preview);
    }

    // ======== Preview Content Loading Tests ========

    #[test]
    fn test_load_preview_content_wraps_lines() {
        let (_temp, storage) = create_test_storage(&[&"x".repeat(200)]);
        let mut picker = Picker::new(storage).unwrap();

        picker.load_preview_content();

        // 200 chars / 80 wrap width = 2-3 lines
        assert!(picker.preview_lines.len() >= 2, "Long line should be wrapped");
    }

    #[test]
    fn test_load_preview_content_multiline() {
        let (_temp, storage) = create_test_storage(&["line1\nline2\nline3"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.load_preview_content();

        assert_eq!(picker.preview_lines.len(), 3);
        assert!(picker.preview_lines.iter().any(|l| l.contains("line1")));
        assert!(picker.preview_lines.iter().any(|l| l.contains("line2")));
        assert!(picker.preview_lines.iter().any(|l| l.contains("line3")));
    }

    #[test]
    fn test_load_preview_content_resets_scroll() {
        let (_temp, storage) = create_test_storage(&["content"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.preview_scroll = 50;
        picker.load_preview_content();

        assert_eq!(picker.preview_scroll, 0);
    }

    // ======== Scroll Calculation Tests ========

    #[test]
    fn test_max_preview_scroll_short_content() {
        let (_temp, storage) = create_test_storage(&["a"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.preview_lines = vec!["line1".to_string(), "line2".to_string()];
        picker.preview_height = 10;

        // 2 lines < 10 height, no scrolling needed
        assert_eq!(picker.max_preview_scroll(), 0);
    }

    #[test]
    fn test_max_preview_scroll_long_content() {
        let (_temp, storage) = create_test_storage(&["a"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.preview_lines = (0..100).map(|i| format!("Line {}", i)).collect();
        picker.preview_height = 10;

        // 100 lines - 10 height = 90 max scroll
        assert_eq!(picker.max_preview_scroll(), 90);
    }

    #[test]
    fn test_max_preview_scroll_exact_fit() {
        let (_temp, storage) = create_test_storage(&["a"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.preview_lines = (0..10).map(|i| format!("Line {}", i)).collect();
        picker.preview_height = 10;

        assert_eq!(picker.max_preview_scroll(), 0);
    }

    // ======== Scroll Bounds Tests ========

    #[test]
    fn test_scroll_saturating_sub_at_zero() {
        let (_temp, storage) = create_test_storage(&["a"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.preview_scroll = 0;
        let new_scroll = picker.preview_scroll.saturating_sub(1);
        assert_eq!(new_scroll, 0);
    }

    #[test]
    fn test_scroll_respects_max() {
        let (_temp, storage) = create_test_storage(&["a"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.preview_lines = (0..100).map(|i| format!("Line {}", i)).collect();
        picker.preview_height = 10;
        picker.preview_scroll = 90;

        assert!(picker.preview_scroll <= picker.max_preview_scroll());
    }

    // ======== Line Wrapping Edge Cases ========

    #[test]
    fn test_wrap_unicode_content() {
        let (_temp, storage) = create_test_storage(&["日本語\n中文\n한국어"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.load_preview_content();

        assert_eq!(picker.preview_lines.len(), 3);
    }

    #[test]
    fn test_wrap_very_long_line() {
        let (_temp, storage) = create_test_storage(&[&"a".repeat(1000)]);
        let mut picker = Picker::new(storage).unwrap();

        picker.load_preview_content();

        // 1000 chars / 80 = 12-13 lines
        assert!(picker.preview_lines.len() >= 12);
    }
}
