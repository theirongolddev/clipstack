use crate::clipboard::Clipboard;
use crate::storage::{ClipEntry, Storage};
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io::stdout;

pub struct Picker {
    storage: Storage,
    entries: Vec<ClipEntry>,
    filtered: Vec<usize>, // Indices into entries
    selected: ListState,
    search_query: String,
    preview_content: Option<String>, // Lazy-loaded preview
    preview_id: Option<String>,
    matcher: SkimMatcherV2,
}

impl Picker {
    pub fn new(storage: Storage) -> Result<Self> {
        let index = storage.load_index()?;

        let mut picker = Self {
            storage,
            entries: index.entries,
            filtered: Vec::new(),
            selected: ListState::default(),
            search_query: String::new(),
            preview_content: None,
            preview_id: None,
            matcher: SkimMatcherV2::default(),
        };

        picker.update_filter();
        if !picker.filtered.is_empty() {
            picker.selected.select(Some(0));
        }

        Ok(picker)
    }

    fn update_filter(&mut self) {
        if self.search_query.is_empty() {
            self.filtered = (0..self.entries.len()).collect();
        } else {
            self.filtered = self
                .entries
                .iter()
                .enumerate()
                .filter_map(|(i, entry)| {
                    self.matcher
                        .fuzzy_match(&entry.preview, &self.search_query)
                        .map(|_| i)
                })
                .collect();
        }

        // Reset selection if needed
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
    }

    fn selected_entry(&self) -> Option<&ClipEntry> {
        self.selected
            .selected()
            .and_then(|i| self.filtered.get(i))
            .and_then(|&idx| self.entries.get(idx))
    }

    fn load_preview(&mut self) {
        // Get entry id first to avoid borrow issues
        let entry_id = self.selected_entry().map(|e| e.id.clone());

        match entry_id {
            Some(id) if self.preview_id.as_ref() != Some(&id) => {
                // Lazy load: only fetch content when selected
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
            _ => {} // Already loaded
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            return;
        }

        let current = self.selected.selected().unwrap_or(0) as i32;
        let new_idx = (current + delta).clamp(0, self.filtered.len() as i32 - 1) as usize;
        self.selected.select(Some(new_idx));
        self.load_preview();
    }

    fn format_size(bytes: usize) -> String {
        if bytes < 1024 {
            format!("{}B", bytes)
        } else if bytes < 1024 * 1024 {
            format!("{:.1}KB", bytes as f64 / 1024.0)
        } else {
            format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
        }
    }

    fn format_time(timestamp: i64) -> String {
        use chrono::{Local, TimeZone};
        if let Some(dt) = Local.timestamp_millis_opt(timestamp).single() {
            dt.format("%H:%M").to_string()
        } else {
            "??:??".to_string()
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Search box
                Constraint::Min(5),    // List + Preview
                Constraint::Length(1), // Help line
            ])
            .split(frame.area());

        // Search box
        let search_block = Block::default().borders(Borders::ALL).title("Search");
        let search_text = Paragraph::new(self.search_query.as_str()).block(search_block);
        frame.render_widget(search_text, chunks[0]);

        // Cursor in search box
        frame.set_cursor_position((chunks[0].x + 1 + self.search_query.len() as u16, chunks[0].y + 1));

        // Split middle into list and preview
        let middle = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(chunks[1]);

        self.render_list(frame, middle[0]);
        self.render_preview(frame, middle[1]);

        // Help line
        let help = Paragraph::new("↑↓:Navigate  Enter:Paste  Esc:Cancel  Ctrl+D:Delete")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(help, chunks[2]);
    }

    fn render_list(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .map(|&idx| {
                let entry = &self.entries[idx];
                let time = Self::format_time(entry.timestamp);
                let size = Self::format_size(entry.size);

                // Truncate preview for list display
                let preview: String = entry
                    .preview
                    .chars()
                    .take(30)
                    .collect::<String>()
                    .replace('\n', " ");

                let line = Line::from(vec![
                    Span::styled(
                        format!("{} ", time),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("[{:>6}] ", size),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::raw(preview),
                ]);

                ListItem::new(line)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!("History ({}/{})", self.filtered.len(), self.entries.len())),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");

        frame.render_stateful_widget(list, area, &mut self.selected);
    }

    fn render_preview(&self, frame: &mut Frame, area: Rect) {
        let content = self
            .preview_content
            .as_deref()
            .unwrap_or("(no selection)");

        // Show first N lines of preview
        let preview_lines: String = content
            .lines()
            .take(100)
            .collect::<Vec<_>>()
            .join("\n");

        let preview = Paragraph::new(preview_lines)
            .block(Block::default().borders(Borders::ALL).title("Preview"))
            .wrap(Wrap { trim: false });

        frame.render_widget(preview, area);
    }

    /// Run the picker UI, returns the selected content or None if cancelled
    pub fn run(&mut self) -> Result<Option<String>> {
        enable_raw_mode()?;
        stdout().execute(EnterAlternateScreen)?;

        let backend = CrosstermBackend::new(stdout());
        let mut terminal = Terminal::new(backend)?;

        // Initial preview load
        self.load_preview();

        let result = self.event_loop(&mut terminal);

        disable_raw_mode()?;
        stdout().execute(LeaveAlternateScreen)?;

        result
    }

    fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    ) -> Result<Option<String>> {
        loop {
            terminal.draw(|f| self.render(f))?;

            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }

                    match key.code {
                        KeyCode::Esc => return Ok(None),

                        KeyCode::Enter => {
                            if let Some(entry) = self.selected_entry() {
                                let content = self.storage.load_content(&entry.id)?;
                                return Ok(Some(content));
                            }
                        }

                        KeyCode::Up => self.move_selection(-1),
                        KeyCode::Down => self.move_selection(1),

                        KeyCode::PageUp => self.move_selection(-10),
                        KeyCode::PageDown => self.move_selection(10),

                        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            // Delete selected entry
                            if let Some(entry) = self.selected_entry().cloned() {
                                self.storage.delete_entry(&entry.id)?;
                                self.entries.retain(|e| e.id != entry.id);
                                self.update_filter();
                                self.load_preview();
                            }
                        }

                        KeyCode::Backspace => {
                            self.search_query.pop();
                            self.update_filter();
                            self.load_preview();
                        }

                        KeyCode::Char(c) => {
                            self.search_query.push(c);
                            self.update_filter();
                            self.load_preview();
                        }

                        _ => {}
                    }
                }
            }
        }
    }
}

/// Run the picker and paste the selected content to clipboard
pub fn pick_and_paste(storage: Storage) -> Result<bool> {
    let mut picker = Picker::new(storage)?;

    if let Some(content) = picker.run()? {
        Clipboard::copy(&content)?;
        println!("Pasted {} bytes to clipboard", content.len());
        Ok(true)
    } else {
        Ok(false)
    }
}
