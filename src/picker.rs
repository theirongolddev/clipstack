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
use std::io::{stdout, Stdout};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Picker mode for vim-style navigation
#[derive(Clone, Copy, PartialEq)]
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

/// Deleted entry for undo functionality
struct DeletedEntry {
    entry: ClipEntry,
    content: String,
    deleted_at: Instant,
}

pub struct Picker {
    storage: Storage,
    entries: Vec<ClipEntry>,
    filtered: Vec<usize>,
    selected: ListState,
    scroll_state: ScrollbarState,
    search_query: String,
    preview_content: Option<String>,
    preview_id: Option<String>,
    matcher: SkimMatcherV2,
    mode: Mode,
    status_message: Option<(String, StatusLevel, Instant)>,
    last_deleted: Option<DeletedEntry>,
    pending_g: bool, // For gg command
}

impl Picker {
    pub fn new(storage: Storage) -> Result<Self> {
        let index = storage.load_index()?;

        let mut picker = Self {
            storage,
            entries: index.entries,
            filtered: Vec::new(),
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
        };

        picker.update_filter();
        if !picker.filtered.is_empty() {
            picker.selected.select(Some(0));
        }
        picker.update_scroll_state();

        Ok(picker)
    }

    fn update_filter(&mut self) {
        if self.search_query.is_empty() {
            self.filtered = (0..self.entries.len()).collect();
        } else {
            // Collect matches with scores
            let mut scored: Vec<(usize, i64)> = self
                .entries
                .iter()
                .enumerate()
                .filter_map(|(i, entry)| {
                    self.matcher
                        .fuzzy_match(&entry.preview, &self.search_query)
                        .map(|score| (i, score))
                })
                .collect();

            // Sort by score descending (best matches first)
            scored.sort_by(|a, b| b.1.cmp(&a.1));

            self.filtered = scored.into_iter().map(|(i, _)| i).collect();
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

    fn move_selection(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            return;
        }

        let current = self.selected.selected().unwrap_or(0) as i32;
        let new_idx = (current + delta).clamp(0, self.filtered.len() as i32 - 1) as usize;
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

            // Store for undo
            self.last_deleted = Some(DeletedEntry {
                entry: entry.clone(),
                content,
                deleted_at: Instant::now(),
            });

            // Delete from storage
            self.storage.delete_entry(&entry.id)?;
            self.entries.retain(|e| e.id != entry.id);
            self.update_filter();
            self.load_preview();

            self.set_status(format!("Deleted '{}' - 'u' to undo (5s)", preview), StatusLevel::Warning);
        }
        Ok(())
    }

    fn undo_delete(&mut self) -> Result<()> {
        if let Some(deleted) = self.last_deleted.take() {
            if deleted.deleted_at.elapsed() < Duration::from_secs(5) {
                // Get preview for status message
                let preview: String = deleted.entry.preview.chars().take(30).collect();

                // Restore the entry
                self.storage.save_entry(&deleted.content)?;

                // Reload entries
                let index = self.storage.load_index()?;
                self.entries = index.entries;
                self.update_filter();
                self.load_preview();

                self.set_status(format!("Restored '{}'", preview), StatusLevel::Success);
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
            .map(|&idx| {
                let entry = &self.entries[idx];
                let time = util::format_relative_time(entry.timestamp);
                let size = util::format_size(entry.size);

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

                ListItem::new(Line::from(spans))
            })
            .collect();

        let title = format!(
            "History ({}/{}){}",
            self.filtered.len(),
            self.entries.len(),
            if !self.search_query.is_empty() {
                format!(" matching '{}'", self.search_query)
            } else {
                String::new()
            }
        );

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

    fn render_preview(&self, frame: &mut Frame, area: Rect) {
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
        let max_lines = (area.height.saturating_sub(2)) as usize;
        let truncated = lines.len() > max_lines;

        let preview_text: String = lines.iter().take(max_lines).copied().collect::<Vec<_>>().join("\n");

        let title = if truncated {
            format!("{} [+{} lines]", metadata, lines.len() - max_lines)
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
            let mode_indicator = match self.mode {
                Mode::Normal => "[NORMAL]",
                Mode::Search => "[SEARCH]",
            };
            (
                format!(
                    "{} j/k:Nav  /:Search  Enter:Paste  d:Delete  u:Undo  G:End  gg:Top  q:Quit",
                    mode_indicator
                ),
                Style::default().fg(Color::DarkGray),
            )
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
