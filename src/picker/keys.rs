//! Key handling per mode and focus.

use super::Picker;
use super::state::{Focus, Mode};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

impl Picker {
    /// Handle keyboard input in Focus::Preview mode
    fn handle_preview_mode(&mut self, key: KeyEvent) -> Result<Option<Option<String>>> {
        let view = &mut self.view;
        match key.code {
            // Line-by-line scrolling
            KeyCode::Up | KeyCode::Char('k') => {
                view.preview_scroll = view.preview_scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max_scroll = view.max_preview_scroll();
                if view.preview_scroll < max_scroll {
                    view.preview_scroll += 1;
                }
            }

            // Page scrolling
            KeyCode::PageUp => {
                view.preview_scroll = view.preview_scroll.saturating_sub(view.preview_height as usize);
            }
            KeyCode::PageDown => {
                let max_scroll = view.max_preview_scroll();
                let page = view.preview_height as usize;
                view.preview_scroll = (view.preview_scroll + page).min(max_scroll);
            }

            // Jump to top/bottom
            KeyCode::Home | KeyCode::Char('g') => {
                view.preview_scroll = 0;
            }
            KeyCode::End | KeyCode::Char('G') => {
                view.preview_scroll = view.max_preview_scroll();
            }

            // Exit preview mode
            KeyCode::Tab | KeyCode::Esc | KeyCode::Char('q') => {
                view.focus = Focus::List;
                view.preview_lines.clear();
                view.preview_scroll = 0;
            }

            _ => {}
        }

        Ok(None)
    }

    pub(super) fn handle_normal_mode(&mut self, key: KeyEvent) -> Result<Option<Option<String>>> {
        // Handle preview scroll navigation when in Focus::Preview mode
        if self.view.focus == Focus::Preview {
            return self.handle_preview_mode(key);
        }

        // Handle pending 'g' for gg command
        if self.view.pending_g {
            self.view.pending_g = false;
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
                    let content = self.data.storage.load_content(entry.id)?;
                    return Ok(Some(Some(content)));
                }
            }

            // Navigation - vim style
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),

            // Page navigation
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => self.move_selection(10),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => self.move_selection(-10),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::PageUp => self.move_selection(-10),

            // Jump to end
            KeyCode::Char('G') => self.jump_to_end(),

            // Jump to start (wait for second g)
            KeyCode::Char('g') => {
                self.view.pending_g = true;
            }

            // Enter search mode
            KeyCode::Char('/') => {
                self.view.mode = Mode::Search;
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
                if self.view.focus == Focus::List {
                    self.load_preview_content();
                    self.view.focus = Focus::Preview;
                } else {
                    self.view.preview_lines.clear();
                    self.view.preview_scroll = 0;
                    self.view.focus = Focus::List;
                }
            }

            // Quick search - any other character starts search
            KeyCode::Char(c) if c.is_alphanumeric() || c == ' ' => {
                self.view.mode = Mode::Search;
                self.view.query.push(c);
                self.update_filter();
                self.load_preview();
            }

            _ => {}
        }

        Ok(None)
    }

    pub(super) fn handle_search_mode(&mut self, key: KeyEvent) -> Result<Option<Option<String>>> {
        match key.code {
            // Exit search mode
            KeyCode::Esc => {
                self.view.mode = Mode::Normal;
            }

            // Select from search
            KeyCode::Enter => {
                if let Some(entry) = self.selected_entry() {
                    let content = self.data.storage.load_content(entry.id)?;
                    return Ok(Some(Some(content)));
                }
            }

            // Navigation in search mode
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),

            // Ctrl+N/P for navigation
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => self.move_selection(1),
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => self.move_selection(-1),

            // Backspace
            KeyCode::Backspace => {
                self.view.query.pop();
                self.update_filter();
                self.load_preview();

                // Exit search mode if query is empty
                if self.view.query.is_empty() {
                    self.view.mode = Mode::Normal;
                }
            }

            // Type characters
            KeyCode::Char(c) => {
                self.view.query.push(c);
                self.update_filter();
                self.load_preview();
            }

            _ => {}
        }

        Ok(None)
    }
}
