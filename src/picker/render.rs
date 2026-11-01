//! Drawing: search box, list, preview pane and status line.

use super::Picker;
use super::data::UNDO_WINDOW;
use super::search::{MatchLocation, highlight_matches};
use super::state::{Focus, Mode, StatusLevel};
use super::text;
use crate::util;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    },
};
use std::time::Duration;

impl Picker {
    pub(super) fn render(&mut self, frame: &mut Frame) {
        // Check for empty state first
        if self.data.entries.is_empty() {
            render_empty_state(frame);
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

    fn render_search_box(&self, frame: &mut Frame, area: Rect) {
        let title = match self.view.mode {
            Mode::Search => "Search (Esc: stop typing, Enter: copy)",
            Mode::Normal => "Search (/ to search)",
        };

        let border_color = match self.view.mode {
            Mode::Search => Color::Cyan,
            Mode::Normal => Color::White,
        };

        let search_block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(border_color));

        let search_text = Paragraph::new(self.view.query.as_str()).block(search_block);
        frame.render_widget(search_text, area);

        // Show cursor only in search mode
        if self.view.mode == Mode::Search {
            // Use character count, not byte length, for correct Unicode cursor position
            let char_count = self.view.query.chars().count() as u16;
            frame.set_cursor_position((area.x + 1 + char_count, area.y + 1));
        }
    }

    fn render_list(&mut self, frame: &mut Frame, area: Rect) {
        let query = &self.view.query;
        let now = crate::storage::now_ms();
        let items: Vec<ListItem> = self
            .search
            .filtered
            .iter()
            .enumerate()
            .map(|(filtered_pos, &idx)| {
                let entry = &self.data.entries[idx];
                let age = text::format_compact_age(entry.last_used, now);
                let size = util::format_size(entry.size);

                // Check if this is a content match (not preview match)
                let is_content_match = self.get_match_location(filtered_pos) == Some(MatchLocation::Content);

                // Truncate preview for list display
                let preview: String = entry.preview.chars().take(30).collect::<String>().replace('\n', " ");

                // Highlight matched characters if searching
                let preview_spans = if !query.is_empty() {
                    highlight_matches(&self.search.matcher, &preview, query)
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
                    Span::styled(format!("{age} "), Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("[{:>5}] ", size), Style::default().fg(Color::Cyan)),
                ];

                // Add [content] indicator for deep matches
                if is_content_match {
                    spans.push(Span::styled("[content] ", Style::default().fg(Color::Magenta)));
                }

                spans.extend(preview_spans);

                ListItem::new(Line::from(spans))
            })
            .collect();

        // Build title with match statistics
        let shown = self.search.filtered.len();
        let total = self.data.entries.len();
        let title = if !query.is_empty() {
            let content_matches = self
                .search
                .results
                .iter()
                .filter(|fe| fe.match_location == MatchLocation::Content)
                .count();
            if content_matches > 0 {
                format!("History ({shown}/{total}) matching '{query}' ({content_matches} in content)")
            } else {
                format!("History ({shown}/{total}) matching '{query}'")
            }
        } else {
            let pinned_count = self.data.entries.iter().filter(|e| e.pinned).count();
            if pinned_count > 0 {
                format!("History ({shown}/{total}) - {pinned_count} pinned")
            } else {
                format!("History ({shown}/{total})")
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

        let selected = self.selected_index();
        let mut list_state = ListState::default().with_offset(self.view.list_offset).with_selected(selected);
        frame.render_stateful_widget(list, area, &mut list_state);
        self.view.list_offset = list_state.offset();

        // Render scrollbar
        let mut scroll_state = ScrollbarState::new(shown).position(selected.unwrap_or(0));
        frame.render_stateful_widget(
            Scrollbar::default()
                .orientation(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("^"))
                .end_symbol(Some("v")),
            area,
            &mut scroll_state,
        );
    }

    /// The selected entry wrapped to the pane. List focus shows its top;
    /// preview focus scrolls through the same rows.
    fn render_preview(&mut self, frame: &mut Frame, area: Rect) {
        // Inside the border
        self.view.preview_height = area.height.saturating_sub(2);
        self.wrap_preview(area.width.saturating_sub(2));

        let view = &self.view;
        let height = usize::from(view.preview_height);
        let total = view.preview_lines.len();
        let start = view.preview_scroll.min(total);
        let end = (start + height).min(total);
        let text = match self.selected_entry() {
            None => "(no selection)".to_string(),
            Some(_) if view.preview_key.is_none() => "(content unavailable)".to_string(),
            Some(_) => view.preview_lines[start..end].join("\n"),
        };

        let (title, border) = if view.focus == Focus::Preview {
            let title = if total > height {
                format!("[PREVIEW] Lines {}-{} of {} (Tab to exit)", start + 1, end, total)
            } else {
                "[PREVIEW] Tab to exit".to_string()
            };
            (title, Color::Yellow)
        } else {
            let mut title = match self.selected_entry() {
                Some(entry) => format!(
                    "Preview - {} - {}",
                    util::format_size(entry.size),
                    util::format_relative_time(entry.last_used)
                ),
                None => "Preview".to_string(),
            };
            if total > height {
                title.push_str(&format!(" [+{} lines, Tab to scroll]", total - height));
            }
            (title, Color::Cyan)
        };

        let preview = Paragraph::new(text).block(
            Block::default().borders(Borders::ALL).title(title).border_style(Style::default().fg(border)),
        );
        frame.render_widget(preview, area);
    }

    /// How long the current status message stays visible. Undo prompts stay
    /// up for the whole undo window instead of vanishing early.
    pub(super) fn status_duration(&self) -> Duration {
        let is_undo_prompt = self.view.status.as_ref().is_some_and(|(msg, _, _)| msg.contains("undo"));
        if is_undo_prompt && self.data.last_deleted.is_some() {
            UNDO_WINDOW
        } else {
            Duration::from_secs(3)
        }
    }

    /// Key help for the status line, based on focus and mode
    pub(super) fn help_text(&self) -> String {
        match (self.view.mode, self.view.focus) {
            (Mode::Search, _) => "[SEARCH] type to filter  ↑/↓:Nav  Enter:Copy  Esc:Done".to_string(),
            (Mode::Normal, Focus::Preview) => {
                "[PREVIEW] j/k:Scroll  PgUp/Dn:Page  g/G:Top/Bottom  w:Whitespace  n:Numbers  Tab/Esc/q:Back".to_string()
            }
            (Mode::Normal, Focus::List) => {
                "[NORMAL] j/k:Nav  /:Search  Tab:Preview  Enter:Copy  p:Pin  dd:Del  u:Undo  q:Quit".to_string()
            }
        }
    }

    fn render_status_line(&mut self, frame: &mut Frame, area: Rect) {
        // Check if we have a status message that hasn't expired
        let ttl = self.status_duration();
        let status_text = if let Some((msg, level, instant)) = &self.view.status {
            let elapsed = instant.elapsed();
            if elapsed < ttl {
                // Show undo countdown if applicable
                let display_msg = if msg.contains("undo") {
                    if let Some(deleted) = &self.data.last_deleted {
                        let remaining = UNDO_WINDOW.as_secs().saturating_sub(deleted.deleted_at.elapsed().as_secs());
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
                self.view.status = None;
                None
            }
        } else {
            None
        };

        let (text, style) = status_text.unwrap_or_else(|| {
            let color = if self.view.focus == Focus::Preview { Color::Yellow } else { Color::DarkGray };
            (self.help_text(), Style::default().fg(color))
        });

        let help = Paragraph::new(text).style(style);
        frame.render_widget(help, area);
    }
}

fn render_empty_state(frame: &mut Frame) {
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
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
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
        Line::from(Span::styled("Press ESC or 'q' to exit", Style::default().fg(Color::Cyan))),
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

#[cfg(test)]
mod tests {
    use super::super::Picker;
    use super::super::tests::create_test_storage;
    use super::*;

    // ======== Status line Tests ========

    #[test]
    fn test_undo_status_lasts_full_undo_window() {
        let (_temp, storage) = create_test_storage(&["a", "b"]);
        let mut picker = Picker::new(storage).unwrap();
        picker.delete_selected().unwrap();
        assert_eq!(picker.status_duration(), Duration::from_secs(5));
    }

    #[test]
    fn test_regular_status_is_short() {
        let (_temp, storage) = create_test_storage(&["a"]);
        let mut picker = Picker::new(storage).unwrap();
        picker.view.set_status("★ Pinned".to_string(), StatusLevel::Success);
        assert_eq!(picker.status_duration(), Duration::from_secs(3));
    }

    use ratatui::{Terminal, backend::TestBackend};

    /// Render `picker` into a `width` x 20 buffer and return its rows.
    pub(in crate::picker) fn draw(picker: &mut Picker, width: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
        terminal.draw(|f| picker.render(f)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn preview_rewraps_on_resize() {
        let (_temp, storage) = create_test_storage(&[&"x".repeat(150)]);
        let mut picker = Picker::new(storage).unwrap();
        for width in [60u16, 120, 60] {
            let rows = draw(&mut picker, width);
            // The preview takes 60% of the width; its border takes 2 columns
            let inner = (width - width * 40 / 100 - 2) as usize;
            let expect = 150usize.div_ceil(inner);
            assert_eq!(picker.view.preview_lines.len(), expect, "width {width}");
            assert_eq!(picker.view.preview_lines[0].len(), inner, "width {width}");
            let pane = (width * 40 / 100) as usize;
            let longest = rows.iter().map(|r| r.chars().skip(pane).filter(|&c| c == 'x').count()).max().unwrap();
            assert_eq!(longest, inner, "a full row of x fills the pane at width {width}");
        }
    }

    #[test]
    fn preview_pane_and_scroll_use_the_same_lines() {
        let (_temp, storage) = create_test_storage(&[&"y".repeat(2000)]);
        let mut picker = Picker::new(storage).unwrap();
        let rows = draw(&mut picker, 120);
        let lines = picker.view.preview_lines.len();
        let shown = picker.view.preview_height as usize;
        assert!(lines > shown);
        assert!(rows[3].contains(&format!("+{} lines", lines - shown)), "{}", rows[3]);
    }

    #[test]
    fn list_time_column_aligned() {
        let (_temp, storage) = create_test_storage(&["a", "b", "c", "d", "e"]);
        let mut picker = Picker::new(storage).unwrap();
        let now = crate::storage::now_ms();
        for (entry, age_s) in picker.data.entries.iter_mut().zip([5, 12 * 60, 3 * 3600, 40 * 86400, 0]) {
            entry.last_used = now - age_s * 1000;
        }
        picker.data.entries[4].last_used = 1; // 1970: capped
        let rows = draw(&mut picker, 120);
        let list: Vec<&String> = rows[4..9].iter().collect();
        let columns: Vec<usize> = list.iter().map(|r| r.chars().position(|c| c == '[').unwrap()).collect();
        assert!(columns.iter().all(|&c| c == columns[0]), "size column moves: {list:#?}");
        let ages: Vec<String> = list.iter().map(|r| r.chars().skip(columns[0] - 5).take(4).collect()).collect();
        assert_eq!(ages, ["  5s", " 12m", "  3h", " 40d", "999d"]);
    }

    #[test]
    fn help_shows_slash_search_and_dd_delete() {
        let (_temp, storage) = create_test_storage(&["a"]);
        let mut picker = Picker::new(storage).unwrap();
        let help = picker.help_text();
        assert!(help.contains("/:Search") && help.contains("dd:Del"), "{help}");
        picker.view.mode = Mode::Search;
        let help = picker.help_text();
        assert!(help.contains("[SEARCH]") && help.contains("Esc"), "{help}");
    }

    #[test]
    fn test_preview_help_does_not_claim_q_quits() {
        let (_temp, storage) = create_test_storage(&["a"]);
        let mut picker = Picker::new(storage).unwrap();
        picker.view.focus = Focus::Preview;
        let help = picker.help_text();
        assert!(!help.contains("q:Quit"), "q exits preview, not the picker: {help}");
        assert!(help.contains("q:Back"));
    }
}
