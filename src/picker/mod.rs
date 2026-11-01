//! The history picker (TUI). `data` holds the entries, `state` the view,
//! `search` the filter, `keys` the key handling and `render` the drawing.

mod actions;
pub use actions::{choose_editor, edit_in_editor, paste_command, spawn_paste};
mod data;
mod keys;
mod render;
mod search;
mod state;
mod text;

use crate::config::PickerConfig;
use crate::daemon::Daemon;
use crate::storage::{ClipEntry, EntryId, IngestOutcome, Kind, ListOrder, Source, Storage};
use anyhow::Result;
use crossterm::{
    ExecutableCommand,
    cursor::Show,
    event::{self, Event, KeyEvent, KeyEventKind},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use keys::Action;
use data::{DeletedEntry, PickerData, UNDO_WINDOW, order_pinned_first};
use ratatui::{Terminal, backend::CrosstermBackend};
use search::{FilteredEntry, IDLE_POLL, MatchLocation, SearchState, search_timeout};
use state::{Focus, Mode, PreviewKey, Prompt, StatusLevel, ViewState};
use std::io::{Stdout, stdout};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How the picker ended, or what the event loop must do for it.
#[derive(Debug, PartialEq)]
enum Outcome {
    /// Edit this entry (the event loop suspends the TUI for the editor)
    Edit(EntryId),
    /// Esc, q or Ctrl+C
    Cancelled,
    /// Enter: this content goes to the clipboard
    Selected(String),
    /// Enter on an image: this PNG goes to the clipboard
    SelectedImage(Vec<u8>),
}

/// What the picker chose: text, or an image's PNG bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    Text(String),
    Image(Vec<u8>),
}

pub struct Picker {
    data: PickerData,
    view: ViewState,
    search: SearchState,
    /// Wait after the last keystroke before searching
    debounce: Duration,
    /// The image cache whose file goes when an image is deleted (`None` in
    /// tests unless they set one, so they never touch the runtime dir)
    image_dir: Option<std::path::PathBuf>,
}

impl Picker {
    pub fn new(storage: Storage) -> Result<Self> {
        let mut picker = Self {
            data: PickerData::new(storage)?,
            view: ViewState::default(),
            search: SearchState::default(),
            debounce: search::DEBOUNCE,
            image_dir: None,
        };

        picker.update_filter();

        Ok(picker)
    }

    fn filter_entries(&self, query: &str) -> Vec<FilteredEntry> {
        search::filter_entries(&self.data, &self.search.matcher, query)
    }

    /// Recompute the visible rows. The selected entry stays selected if it
    /// is still visible; otherwise the first row is (none if no rows).
    fn update_filter(&mut self) {
        if self.view.query.is_empty() {
            // No search query - show all entries in order
            self.search.filtered = (0..self.data.entries.len()).collect();
            self.search.results.clear();
        } else {
            // Run two-phase search
            self.search.runs += 1;
            self.search.results = self.filter_entries(&self.view.query);

            // Extract indices for filtered list
            self.search.filtered = self.search.results.iter().map(|e| e.index).collect();
        }

        if self.selected_index().is_none() {
            self.select_index(0);
        }
        if self.view.selection.is_none() {
            self.data.content_cache = None;
        }
    }

    /// The row of the selected entry among the visible rows.
    fn selected_index(&self) -> Option<usize> {
        let id = self.view.selection?;
        self.search.filtered.iter().position(|&idx| self.data.entries[idx].id == id)
    }

    /// Select the entry on row `i`, or the last row if `i` is past the end
    /// (nothing if there are no rows).
    fn select_index(&mut self, i: usize) {
        let row = i.min(self.search.filtered.len().saturating_sub(1));
        self.view.selection = self.search.filtered.get(row).map(|&idx| self.data.entries[idx].id);
    }

    fn selected_entry(&self) -> Option<&ClipEntry> {
        self.selected_index()
            .and_then(|i| self.search.filtered.get(i))
            .and_then(|&idx| self.data.entries.get(idx))
    }

    /// Toggle pin status of selected entry. Pinning follows the entry to the
    /// pinned group; unpinning keeps the cursor on its row (FEATURE_PLAN
    /// Feature 4), so the next entry is selected.
    fn toggle_pin_selected(&mut self) -> Result<()> {
        let Some(row) = self.selected_index() else {
            return Ok(());
        };
        let idx = self.search.filtered[row];
        let entry_id = self.data.entries[idx].id;

        match self.data.storage.toggle_pin(entry_id) {
            Ok(is_pinned) => {
                // Update local state (unpinning drops the alias)
                self.data.entries[idx].pinned = is_pinned;
                let dropped = if is_pinned { None } else { self.data.entries[idx].alias.take() };

                // Re-sort: pinned entries first
                self.sort_entries_by_pin();
                if !is_pinned {
                    self.select_index(row);
                    self.load_preview();
                }

                match (is_pinned, dropped) {
                    (true, _) => self.view.set_status("★ Pinned".to_string(), StatusLevel::Success),
                    (false, Some(alias)) => self
                        .view
                        .set_status(format!("Unpinned (alias @{alias} removed)"), StatusLevel::Warning),
                    (false, None) => self.view.set_status("Unpinned".to_string(), StatusLevel::Success),
                }
            }
            Err(e) => {
                self.view.set_status(e.to_string(), StatusLevel::Warning);
            }
        }
        Ok(())
    }

    /// Sort entries: pinned first, each group kept in recency (index) order.
    /// The selection is an entry id, so it follows its entry.
    /// Edit entry `id` with `editor` (temp file in `dir`). New text is saved
    /// (source `edit`) and selected, which copies it and closes the picker;
    /// otherwise the picker stays open with a status message.
    fn edit_entry(&mut self, id: EntryId, editor: &str, dir: &std::path::Path) -> Result<Option<Outcome>> {
        let edited = self
            .data
            .storage
            .load_content(id)
            .and_then(|content| actions::edit_in_editor(&content, editor, dir, id));
        match edited {
            Ok(Some(text)) => {
                if let IngestOutcome::Skipped(reason) = self.data.storage.ingest(&text, Source::Edit, &[])? {
                    // Still copied, as with `clipstack copy`
                    eprintln!("Edited text not saved to history: {}", self.data.storage.policy().describe_skip(&reason));
                }
                Ok(Some(Outcome::Selected(text)))
            }
            Ok(None) => {
                self.view.set_status(
                    "Edit: not changed (unchanged, empty, or the editor failed)".to_string(),
                    StatusLevel::Warning,
                );
                Ok(None)
            }
            Err(e) => {
                self.view.set_status(format!("Edit failed: {e:#}"), StatusLevel::Warning);
                Ok(None)
            }
        }
    }

    /// Save the prompt's alias (pinning the entry), or remove the alias if
    /// the input is empty. A refused name (invalid, taken, pin limit) shows
    /// in the status line and leaves the prompt open to fix it.
    fn submit_alias(&mut self) {
        let Some(prompt) = &self.view.prompt else {
            return;
        };
        let id = prompt.id;
        let input = prompt.input.trim();
        let name = input.strip_prefix('@').unwrap_or(input).to_string();
        let Some(idx) = self.data.entries.iter().position(|e| e.id == id) else {
            self.view.prompt = None;
            return;
        };
        let alias = if name.is_empty() { None } else { Some(name.as_str()) };
        if alias.is_none() && self.data.entries[idx].alias.is_none() {
            self.view.prompt = None;
            return;
        }
        if let Err(e) = self.data.storage.set_alias(id, alias) {
            self.view.set_status(e.to_string(), StatusLevel::Warning);
            return;
        }
        self.view.prompt = None;
        let entry = &mut self.data.entries[idx];
        match alias {
            Some(name) => {
                entry.pinned = true;
                entry.alias = Some(name.to_string());
                self.view.set_status(format!("★ Pinned as @{name}"), StatusLevel::Success);
                self.sort_entries_by_pin();
            }
            None => {
                let old = entry.alias.take().unwrap_or_default();
                self.view.set_status(format!("Alias @{old} removed (still pinned)"), StatusLevel::Success);
            }
        }
    }

    fn sort_entries_by_pin(&mut self) {
        order_pinned_first(&mut self.data.entries);
        self.update_filter();
        self.load_preview();
    }

    /// Get the match location for a filtered entry position
    /// Returns None if no search is active or position is out of bounds
    fn get_match_location(&self, filtered_pos: usize) -> Option<MatchLocation> {
        if self.view.query.is_empty() {
            return None;
        }
        self.search.results.get(filtered_pos).map(|e| e.match_location)
    }

    fn load_preview(&mut self) {
        let entry_id = self.selected_entry().map(|e| e.id);

        match entry_id {
            Some(id) if self.data.cached_content(id).is_none() => {
                let image = self.selected_entry().filter(|e| e.kind == Kind::Image).map(image_description);
                match image.map_or_else(|| self.data.storage.load_content(id), Ok) {
                    Ok(content) => self.data.content_cache = Some((id, content)),
                    Err(_) => self.data.content_cache = None,
                }
            }
            None => self.data.content_cache = None,
            _ => {}
        }
    }

    /// Load and wrap preview content for Focus::Preview mode
    /// Lay out the selected entry at `width` columns into `view.preview_lines`
    /// (kept until the entry, the width or a display toggle changes). Another entry starts
    /// at its top.
    fn wrap_preview(&mut self, width: u16) {
        self.load_preview();
        let Some((id, content)) = &self.data.content_cache else {
            self.view.preview_lines.clear();
            self.view.preview_key = None;
            return;
        };
        let key = PreviewKey {
            id: *id,
            width,
            show_whitespace: self.view.show_whitespace,
            show_line_numbers: self.view.show_line_numbers,
        };
        if self.view.preview_key == Some(key) {
            return;
        }
        if self.view.preview_key.map(|old| old.id) != Some(*id) {
            self.view.preview_scroll = 0;
        }
        self.view.preview_lines =
            text::layout_preview(content, usize::from(width), key.show_whitespace, key.show_line_numbers);
        self.view.preview_key = Some(key);
        self.view.preview_scroll = self.view.preview_scroll.min(self.view.max_preview_scroll());
    }

    fn move_selection(&mut self, delta: isize) {
        let Some(current) = self.selected_index() else {
            return;
        };
        // The preview is rewrapped (from the top) for the new entry at render
        self.select_index(current.saturating_add_signed(delta));
        self.load_preview();
    }

    fn jump_to_start(&mut self) {
        self.select_index(0);
        self.load_preview();
    }

    fn jump_to_end(&mut self) {
        self.select_index(usize::MAX);
        self.load_preview();
    }

    /// Format a status message with optional pin indicator
    fn format_entry_message(action: &str, preview: &str, pinned: bool, suffix: &str) -> String {
        let pin = if pinned { "★ " } else { "" };
        if suffix.is_empty() {
            format!("{} {}'{}'", action, pin, preview)
        } else {
            format!("{} {}'{}' - {}", action, pin, preview, suffix)
        }
    }

    fn delete_selected(&mut self) -> Result<()> {
        if let Some(entry) = self.selected_entry().cloned() {
            let content = match entry.kind {
                Kind::Image => Picked::Image(self.data.storage.load_image(entry.id)?),
                Kind::Text => Picked::Text(self.data.storage.load_content(entry.id)?),
            };
            let preview: String = entry.preview.chars().take(30).collect();
            let was_pinned = entry.pinned;

            self.data.last_deleted = Some(DeletedEntry {
                entry: entry.clone(),
                content,
                was_pinned,
                deleted_at: Instant::now(),
            });

            let row = self.selected_index().unwrap_or(0);
            self.data.storage.delete_entry(entry.id)?;
            if entry.kind == Kind::Image
                && let Some(dir) = &self.image_dir
            {
                crate::image_cache::remove_in(dir, entry.id);
            }
            self.data.entries.retain(|e| e.id != entry.id);
            // The entry is gone: select whatever is now on its row
            self.view.selection = None;
            self.update_filter();
            self.select_index(row);
            self.load_preview();

            let msg = Self::format_entry_message("Deleted", &preview, was_pinned, "'u' to undo (5s)");
            self.view.set_status(msg, StatusLevel::Warning);
        }
        Ok(())
    }

    /// `result`, except that an entry deleted or expired since the list was
    /// read (`StorageError::NotFound`) reloads the list with a status
    /// message instead of ending the picker with an error.
    fn unless_gone<T>(&mut self, result: Result<T>) -> Result<Option<T>> {
        match result {
            Ok(value) => Ok(Some(value)),
            Err(e) if matches!(e.downcast_ref(), Some(crate::storage::StorageError::NotFound { .. })) => {
                let row = self.selected_index().unwrap_or(0);
                self.data.entries = self.data.storage.list(ListOrder::PinnedFirst, None)?;
                self.view.selection = None;
                self.update_filter();
                self.select_index(row);
                self.data.content_cache = None;
                self.load_preview();
                self.view.set_status(
                    "That entry is gone (deleted or expired since the list was read)".to_string(),
                    StatusLevel::Warning,
                );
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn undo_delete(&mut self) -> Result<()> {
        if let Some(deleted) = self.data.last_deleted.take() {
            if deleted.deleted_at.elapsed() < UNDO_WINDOW {
                let preview: String = deleted.entry.preview.chars().take(30).collect();
                let restored = match &deleted.content {
                    Picked::Text(text) => self.data.storage.ingest(text, Source::Picker, &[])?,
                    Picked::Image(png) => self.data.storage.ingest_image(png, Source::Picker, &[])?,
                };
                let restored = match restored {
                    IngestOutcome::Inserted(entry) | IngestOutcome::Bumped(entry) => entry,
                    IngestOutcome::Skipped(reason) => {
                        let reason = self.data.storage.policy().describe_skip(&reason);
                        self.view.set_status(format!("Can't restore: {reason}"), StatusLevel::Warning);
                        return Ok(());
                    }
                };

                // Best effort: the pin limit or a reused alias may refuse
                if deleted.was_pinned {
                    let _ = self.data.storage.set_pinned(restored.id, true);
                }
                if let Some(alias) = deleted.entry.alias.as_deref() {
                    let _ = self.data.storage.set_alias(restored.id, Some(alias));
                }

                self.data.entries = self.data.storage.list(ListOrder::PinnedFirst, None)?;
                self.view.selection = Some(restored.id);
                self.update_filter();
                self.load_preview();

                let msg = Self::format_entry_message("Restored", &preview, deleted.was_pinned, "");
                self.view.set_status(msg, StatusLevel::Success);
            } else {
                self.view.set_status("Undo expired".to_string(), StatusLevel::Warning);
            }
        }
        Ok(())
    }

    /// Map `key` to an action and apply it.
    fn handle_key(&mut self, key: KeyEvent) -> Result<Option<Outcome>> {
        let action = keys::map_key(&self.view, key);
        self.apply(action)
    }

    /// Apply one action; `Some` ends the picker.
    fn apply(&mut self, action: Action) -> Result<Option<Outcome>> {
        self.apply_at(action, Instant::now())
    }

    /// Run the pending search if it is due at `now`.
    fn tick(&mut self, now: Instant) {
        if search_timeout(self.view.pending_query_at, now, self.debounce, IDLE_POLL).is_zero() {
            self.run_pending_search();
        }
    }

    /// Run the search for the current query now, if one is waiting.
    fn run_pending_search(&mut self) {
        if self.view.pending_query_at.take().is_some() {
            self.update_filter();
            self.load_preview();
        }
    }

    /// The query changed at `now`: search once typing pauses (an empty
    /// query lists everything, so it needs no wait).
    fn query_changed(&mut self, now: Instant) {
        self.view.pending_query_at = Some(now);
        if self.view.query.is_empty() || self.debounce.is_zero() {
            self.run_pending_search();
        }
    }

    /// `apply` at a given time (tests inject it to check the debounce).
    fn apply_at(&mut self, action: Action, now: Instant) -> Result<Option<Outcome>> {
        self.view.pending = None;
        // Every other action acts on the rows, so they must match the query
        if !matches!(action, Action::None | Action::SearchInput(_) | Action::SearchBackspace) {
            self.run_pending_search();
        }
        match action {
            Action::None => {}
            Action::Quit => return Ok(Some(Outcome::Cancelled)),
            Action::Select => {
                if let Some(entry) = self.selected_entry() {
                    let outcome = match entry.kind {
                        Kind::Image => self.data.storage.load_image(entry.id).map(Outcome::SelectedImage),
                        Kind::Text => self.data.storage.load_content(entry.id).map(Outcome::Selected),
                    };
                    return self.unless_gone(outcome);
                }
            }
            Action::Move(delta) => self.move_selection(delta),
            Action::JumpTop => self.jump_to_start(),
            Action::JumpBottom => self.jump_to_end(),
            Action::Pending(key) => {
                self.view.pending = Some(key);
                if key == 'd' && self.selected_entry().is_some() {
                    self.view.set_status("Press d again to delete".to_string(), StatusLevel::Warning);
                }
            }
            Action::EnterSearch => self.view.mode = Mode::Search,
            Action::ExitSearch => self.view.mode = Mode::Normal,
            Action::SearchInput(c) => {
                self.view.query.push(c);
                self.query_changed(now);
            }
            Action::SearchBackspace => {
                self.view.query.pop();
                self.query_changed(now);
                // Exit search mode if query is empty
                if self.view.query.is_empty() {
                    self.view.mode = Mode::Normal;
                }
            }
            Action::Delete => {
                let deleted = self.delete_selected();
                self.unless_gone(deleted)?;
            }
            Action::Undo => self.undo_delete()?,
            Action::TogglePin => self.toggle_pin_selected()?,
            Action::FocusPreview => {
                self.view.focus = Focus::Preview;
                self.view.preview_scroll = 0;
            }
            Action::FocusList => {
                self.view.focus = Focus::List;
                self.view.preview_scroll = 0;
            }
            Action::Scroll(lines) => self.scroll_preview(lines),
            Action::ScrollPage(pages) => {
                self.scroll_preview(pages.saturating_mul(self.view.preview_height as isize));
            }
            Action::ScrollTop => self.view.preview_scroll = 0,
            Action::ScrollBottom => self.view.preview_scroll = self.view.max_preview_scroll(),
            Action::ToggleWhitespace => self.view.show_whitespace = !self.view.show_whitespace,
            Action::ToggleLineNumbers => self.view.show_line_numbers = !self.view.show_line_numbers,
            Action::OpenAliasPrompt => {
                if let Some(entry) = self.selected_entry() {
                    let input = entry.alias.clone().unwrap_or_default();
                    self.view.prompt = Some(Prompt { id: entry.id, input });
                }
            }
            Action::PromptInput(c) => {
                if let Some(prompt) = &mut self.view.prompt {
                    prompt.input.push(c);
                }
            }
            Action::PromptBackspace => {
                if let Some(prompt) = &mut self.view.prompt {
                    prompt.input.pop();
                }
            }
            Action::PromptSubmit => self.submit_alias(),
            Action::PromptCancel => self.view.prompt = None,
            Action::Edit => match self.selected_entry() {
                Some(entry) if entry.kind == Kind::Image => {
                    self.view.set_status("Images can't be edited (Enter copies it)".to_string(), StatusLevel::Warning);
                }
                Some(entry) => return Ok(Some(Outcome::Edit(entry.id))),
                None => {}
            },
        }
        Ok(None)
    }

    /// Scroll the preview by `lines`, within its bounds.
    fn scroll_preview(&mut self, lines: isize) {
        let view = &mut self.view;
        view.preview_scroll = view.preview_scroll.saturating_add_signed(lines).min(view.max_preview_scroll());
    }

    pub fn run(&mut self) -> Result<Option<Picked>> {
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

    /// Leave the TUI, edit entry `id` in `$VISUAL`/`$EDITOR`, come back.
    fn edit_suspended(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>, id: EntryId) -> Result<Option<Outcome>> {
        let dir = match crate::daemon::runtime_dir() {
            Ok(dir) => dir,
            Err(e) => {
                self.view.set_status(format!("Edit failed: {e:#}"), StatusLevel::Warning);
                return Ok(None);
            }
        };
        let env = |name| std::env::var(name).ok();
        let editor = actions::choose_editor(env("VISUAL").as_deref(), env("EDITOR").as_deref());
        suspend_tui(terminal)?;
        let outcome = self.edit_entry(id, &editor, &dir);
        resume_tui(terminal)?;
        outcome
    }

    fn event_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<Option<Picked>> {
        loop {
            self.tick(Instant::now());
            terminal.draw(|f| self.render(f))?;

            let timeout = search_timeout(self.view.pending_query_at, Instant::now(), self.debounce, IDLE_POLL);
            if event::poll(timeout)?
                && let Event::Key(key) = event::read()?
            {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                let mut outcome = self.handle_key(key)?;
                if let Some(Outcome::Edit(id)) = outcome {
                    outcome = self.edit_suspended(terminal, id)?;
                }
                match outcome {
                    Some(Outcome::Selected(content)) => return Ok(Some(Picked::Text(content))),
                    Some(Outcome::SelectedImage(png)) => return Ok(Some(Picked::Image(png))),
                    Some(Outcome::Cancelled) => return Ok(None),
                    Some(Outcome::Edit(_)) | None => {}
                }
            }

            // Clear expired undo
            if let Some(deleted) = &self.data.last_deleted
                && deleted.deleted_at.elapsed() >= UNDO_WINDOW
            {
                self.data.last_deleted = None;
            }
        }
    }
}
/// Hand the terminal back (for an editor): normal screen, cooked mode.
fn suspend_tui(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    terminal.backend_mut().execute(Show)?;
    disable_raw_mode()?;
    Ok(())
}

/// Take the terminal back after `suspend_tui` and redraw everything.
fn resume_tui(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    enable_raw_mode()?;
    terminal.backend_mut().execute(EnterAlternateScreen)?;
    terminal.clear()?;
    Ok(())
}

/// Arguments for auto-spawning the daemon against the picker's storage.
/// The limit is deliberately omitted so the daemon reuses the saved one.
fn daemon_spawn_args(storage_dir: &std::path::Path) -> Vec<std::ffi::OsString> {
    vec![
        "--storage-dir".into(),
        storage_dir.as_os_str().to_owned(),
        "daemon".into(),
    ]
}

/// Program to auto-spawn as the daemon: the running executable, from
/// `std::env::current_exe()`. Never a `clipstack` looked up on PATH, which
/// could be a different (installed) version than the one running. `None`
/// if the executable can't be resolved; the picker then runs without one.
fn daemon_spawn_program(
    current_exe: std::io::Result<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    current_exe.ok()
}

/// The preview pane's text for an image entry (images are not drawn).
fn image_description(entry: &ClipEntry) -> String {
    let (width, height) = (entry.width.unwrap_or(0), entry.height.unwrap_or(0));
    format!(
        "PNG image, {width}x{height} pixels, {}\n\nEnter copies the image; `clipstack get id:{} > file.png` saves it.",
        crate::util::format_size(entry.size),
        entry.id
    )
}

/// Ensure daemon is running, silently spawning if needed
fn ensure_daemon_running(storage_dir: &std::path::Path) {
    if Daemon::is_running() {
        return;
    }

    let Some(program) = daemon_spawn_program(std::env::current_exe()) else {
        return;
    };

    // Silently spawn daemon
    let _ = Command::new(program)
        .args(daemon_spawn_args(storage_dir))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    // Brief wait for daemon to start
    std::thread::sleep(Duration::from_millis(200));
}

/// Run the picker (starting the daemon if needed); the chosen content, or
/// `None` if the user cancelled.
pub fn pick(storage: Storage, config: &PickerConfig) -> Result<Option<Picked>> {
    ensure_daemon_running(storage.base_dir());
    let mut picker = Picker::new(storage)?;
    picker.debounce = config.search_debounce();
    picker.image_dir = crate::image_cache::dir().ok();
    picker.run()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::storage::IngestPolicy;
    use crossterm::event::{KeyCode, KeyModifiers};
    use tempfile::TempDir;

    // Helper: Create a test storage with entries, oldest first
    pub(in crate::picker) fn create_test_storage(entries: &[&str]) -> (TempDir, Storage) {
        let temp = TempDir::new().unwrap();
        let storage = Storage::open(temp.path().to_path_buf(), Some(100), IngestPolicy::default()).unwrap();
        for (i, content) in entries.iter().enumerate() {
            storage.ingest_at(content, Source::Copy, &[], i as i64 + 1).unwrap();
        }
        (temp, storage)
    }

    fn previews(picker: &Picker) -> Vec<&str> {
        picker.search.filtered.iter().map(|&i| picker.data.entries[i].preview.as_str()).collect()
    }

    // ======== Pin ordering Tests ========

    #[test]
    fn test_pinned_entries_shown_first_on_open() {
        let (_temp, storage) = create_test_storage(&["oldest", "middle", "newest"]);
        let oldest_id = storage.list(ListOrder::Recency, None).unwrap()[2].id;
        storage.toggle_pin(oldest_id).unwrap();

        let picker = Picker::new(storage).unwrap();
        // Pinned first, then the rest in recency order
        assert_eq!(previews(&picker), vec!["oldest", "newest", "middle"]);
    }

    // ======== Undo Tests ========

    #[test]
    fn undo_restores_deleted_entry_with_pin_and_alias() {
        let (_temp, storage) = create_test_storage(&["keep", "signature"]);
        let sig = storage.list(ListOrder::Recency, None).unwrap()[0].id;
        storage.set_alias(sig, Some("sig")).unwrap();
        let mut picker = Picker::new(storage).unwrap();
        assert_eq!(picker.selected_entry().unwrap().preview, "signature");

        picker.delete_selected().unwrap();
        assert_eq!(previews(&picker), vec!["keep"]);
        assert_eq!(picker.data.storage.get(sig).unwrap(), None, "deleted from storage");

        picker.undo_delete().unwrap();
        assert_eq!(previews(&picker), vec!["signature", "keep"]);
        let restored = picker.data.storage.by_alias("sig").unwrap().expect("alias restored");
        assert!(restored.pinned);
        assert_eq!(restored.source, Source::Picker);
        assert_eq!(picker.data.storage.load_content(restored.id).unwrap(), "signature");
    }

    #[test]
    fn undo_restores_unpinned_entry_unpinned() {
        let (_temp, storage) = create_test_storage(&["a", "b"]);
        let mut picker = Picker::new(storage).unwrap();
        picker.delete_selected().unwrap();
        picker.undo_delete().unwrap();
        let entries = picker.data.storage.list(ListOrder::Recency, None).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| !e.pinned));
    }

    #[test]
    fn undo_after_window_does_not_restore() {
        let (_temp, storage) = create_test_storage(&["a", "b"]);
        let mut picker = Picker::new(storage).unwrap();
        picker.delete_selected().unwrap();
        let deleted = picker.data.last_deleted.as_mut().unwrap();
        deleted.deleted_at = Instant::now() - UNDO_WINDOW - Duration::from_secs(1);
        picker.undo_delete().unwrap();
        assert_eq!(picker.data.storage.counts().unwrap().total, 1);
    }

    /// Press each key of `keys` as a plain character.
    fn press(picker: &mut Picker, keys: &str) -> Option<Outcome> {
        let mut outcome = None;
        for c in keys.chars() {
            let mods = if c.is_ascii_uppercase() { KeyModifiers::SHIFT } else { KeyModifiers::NONE };
            let code = if c == '\t' { KeyCode::Tab } else { KeyCode::Char(c) };
            outcome = picker.handle_key(KeyEvent::new(code, mods)).unwrap();
        }
        outcome
    }

    #[test]
    fn dd_deletes_and_u_restores() {
        let (_temp, storage) = create_test_storage(&["keep", "gone"]);
        let mut picker = Picker::new(storage).unwrap();
        press(&mut picker, "d");
        assert_eq!(previews(&picker), vec!["gone", "keep"], "one d deletes nothing");
        press(&mut picker, "d");
        assert_eq!(previews(&picker), vec!["keep"]);
        press(&mut picker, "u");
        assert_eq!(previews(&picker), vec!["gone", "keep"]);
        assert_eq!(picker.data.storage.counts().unwrap().total, 2);
    }

    #[test]
    fn slash_then_typing_filters_and_esc_keeps_query() {
        let (_temp, storage) = create_test_storage(&["docker ps", "git log"]);
        let mut picker = Picker::new(storage).unwrap();
        press(&mut picker, "docker");
        assert_eq!(picker.data.storage.counts().unwrap().total, 2, "typing in normal mode deletes nothing");
        assert!(picker.view.query.is_empty());

        press(&mut picker, "/dock");
        assert_eq!(picker.view.mode, Mode::Search);
        picker.tick(Instant::now() + search::DEBOUNCE);
        assert_eq!(previews(&picker), vec!["docker ps"]);
        picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).unwrap();
        assert_eq!(picker.view.mode, Mode::Normal);
        assert_eq!(previews(&picker), vec!["docker ps"], "Esc leaves search mode, the filter stays");
        assert_eq!(press(&mut picker, "q"), Some(Outcome::Cancelled));
    }

    fn key(picker: &mut Picker, code: KeyCode) -> Option<Outcome> {
        picker.handle_key(KeyEvent::new(code, KeyModifiers::NONE)).unwrap()
    }

    fn status(picker: &Picker) -> &str {
        picker.view.status.as_ref().map_or("", |(msg, _, _)| msg.as_str())
    }

    #[test]
    fn acting_on_an_entry_removed_meanwhile_keeps_the_picker_open() {
        // The daemon expired or evicted it, or another clipstack deleted it
        for keys in ["\n", "dd"] {
            let (_temp, storage) = create_test_storage(&["keep", "gone"]);
            let mut picker = Picker::new(storage).unwrap();
            let gone = picker.selected_entry().unwrap().id;
            picker.data.storage.delete_entry(gone).unwrap();
            let outcome = if keys == "\n" { key(&mut picker, KeyCode::Enter) } else { press(&mut picker, keys) };
            assert_eq!(outcome, None, "{keys:?}");
            assert!(status(&picker).contains("gone"), "{keys:?}: {}", status(&picker));
            assert_eq!(previews(&picker), vec!["keep"], "{keys:?}: the list is reloaded");
            assert_eq!(key(&mut picker, KeyCode::Enter), Some(Outcome::Selected("keep".to_string())));
        }
    }

    #[test]
    fn alias_prompt_sets_alias_and_pins() {
        let (_temp, storage) = create_test_storage(&["old", "new"]);
        let mut picker = Picker::new(storage).unwrap();
        let id = picker.selected_entry().unwrap().id;
        press(&mut picker, "P");
        assert!(picker.view.prompt.is_some(), "P opens the alias prompt");
        // Every letter is text in the prompt (q does not quit, d does not arm)
        assert_eq!(press(&mut picker, "@qd-1"), None);
        key(&mut picker, KeyCode::Enter);
        assert!(picker.view.prompt.is_none());
        let entry = picker.data.storage.by_alias("qd-1").unwrap().expect("alias saved");
        assert_eq!(entry.id, id);
        assert!(entry.pinned, "naming pins");
        assert_eq!(picker.selected_entry().map(|e| (e.id, e.alias.as_deref())), Some((id, Some("qd-1"))));
        assert!(status(&picker).contains("@qd-1"), "{}", status(&picker));

        // Reopening shows the alias; clearing it removes the alias and keeps the pin
        press(&mut picker, "P");
        assert_eq!(picker.view.prompt.as_ref().map(|p| p.input.as_str()), Some("qd-1"));
        for _ in 0..4 {
            key(&mut picker, KeyCode::Backspace);
        }
        key(&mut picker, KeyCode::Enter);
        let entry = picker.data.storage.get(id).unwrap().unwrap();
        assert_eq!((entry.alias, entry.pinned), (None, true));
        assert_eq!(picker.selected_entry().unwrap().alias, None);

        // Esc cancels without saving
        press(&mut picker, "Pzzz");
        key(&mut picker, KeyCode::Esc);
        assert!(picker.view.prompt.is_none());
        assert_eq!(picker.data.storage.by_alias("zzz").unwrap(), None);
        assert_eq!(picker.view.mode, Mode::Normal);
    }

    #[test]
    fn invalid_alias_shows_error() {
        let (_temp, storage) = create_test_storage(&["old", "new"]);
        let mut picker = Picker::new(storage).unwrap();
        press(&mut picker, "PBad");
        key(&mut picker, KeyCode::Enter);
        assert!(picker.view.prompt.is_some(), "the prompt stays open to fix the name");
        assert!(status(&picker).contains("1-32 characters"), "{}", status(&picker));
        assert!(!picker.selected_entry().unwrap().pinned, "nothing saved");

        key(&mut picker, KeyCode::Esc);
        press(&mut picker, "Pwork");
        key(&mut picker, KeyCode::Enter);
        press(&mut picker, "jPwork");
        key(&mut picker, KeyCode::Enter);
        assert!(status(&picker).contains("already used"), "{}", status(&picker));
        assert!(picker.view.prompt.is_some());
    }

    #[test]
    fn unpin_warns_that_the_alias_goes() {
        let (_temp, storage) = create_test_storage(&["a"]);
        let mut picker = Picker::new(storage).unwrap();
        press(&mut picker, "Pwork");
        key(&mut picker, KeyCode::Enter);
        press(&mut picker, "p");
        assert_eq!(status(&picker), "Unpinned (alias @work removed)");
        assert_eq!(picker.data.storage.by_alias("work").unwrap(), None);
    }

    /// Storage holding text "older" and then a 640x480 image (the newest).
    fn storage_with_image() -> (TempDir, Storage, Vec<u8>) {
        let (temp, storage) = create_test_storage(&["older"]);
        let png = crate::png::test_png(640, 480, b"picker");
        storage.ingest_image_at(&png, Source::Watch, &[], 10).unwrap();
        (temp, storage, png)
    }

    #[test]
    fn enter_on_an_image_selects_its_png() {
        let (_temp, storage, png) = storage_with_image();
        let mut picker = Picker::new(storage).unwrap();
        assert_eq!(picker.selected_entry().unwrap().preview, "Image \u{b7} 640x480");
        assert_eq!(key(&mut picker, KeyCode::Enter), Some(Outcome::SelectedImage(png)));
    }

    #[test]
    fn e_on_an_image_explains_instead_of_editing() {
        let (_temp, storage, _png) = storage_with_image();
        let mut picker = Picker::new(storage).unwrap();
        assert_eq!(press(&mut picker, "e"), None);
        assert!(status(&picker).contains("can't be edited"), "{}", status(&picker));
    }

    #[test]
    fn image_preview_describes_the_image() {
        let (_temp, storage, png) = storage_with_image();
        let mut picker = Picker::new(storage).unwrap();
        picker.load_preview();
        let id = picker.selected_entry().unwrap().id;
        let text = picker.data.cached_content(id).expect("a preview text");
        assert!(text.contains("640x480") && text.contains("PNG"), "{text}");
        assert!(text.contains(&crate::util::format_size(png.len())), "{text}");
    }

    #[test]
    fn deleting_an_image_can_be_undone_and_removes_its_cached_file() {
        let (_temp, storage, png) = storage_with_image();
        let mut picker = Picker::new(storage).unwrap();
        let cache = TempDir::new().unwrap();
        picker.image_dir = Some(cache.path().to_path_buf());
        let id = picker.selected_entry().unwrap().id;
        let cached = cache.path().join(format!("{id}.png"));
        std::fs::write(&cached, &png).unwrap();

        picker.delete_selected().unwrap();
        assert!(!cached.exists(), "the cached file goes with the entry");
        picker.undo_delete().unwrap();
        let restored = picker.selected_entry().unwrap().clone();
        assert_eq!((restored.width, restored.height), (Some(640), Some(480)));
        assert_eq!(picker.data.storage.load_image(restored.id).unwrap(), png);
    }

    #[test]
    fn e_edits_then_saves_and_selects_the_new_text() {
        let (_temp, storage) = create_test_storage(&["other", "before"]);
        let mut picker = Picker::new(storage).unwrap();
        let dir = TempDir::new().unwrap();
        assert_eq!(press(&mut picker, "e"), Some(Outcome::Edit(picker.selected_entry().unwrap().id)));

        let id = picker.selected_entry().unwrap().id;
        let outcome = picker.edit_entry(id, "sh -c 'printf edited > \"$1\"' --", dir.path()).unwrap();
        assert_eq!(outcome, Some(Outcome::Selected("edited".to_string())));
        let saved = picker.data.storage.nth(1).unwrap().unwrap();
        assert_eq!((saved.preview.as_str(), saved.source), ("edited", Source::Edit));
        assert_eq!(picker.data.storage.counts().unwrap().total, 3, "the original stays");
    }

    #[test]
    fn edit_without_change_stays_in_the_picker() {
        let (_temp, storage) = create_test_storage(&["before"]);
        let mut picker = Picker::new(storage).unwrap();
        let dir = TempDir::new().unwrap();
        let id = picker.selected_entry().unwrap().id;
        assert_eq!(picker.edit_entry(id, "true", dir.path()).unwrap(), None);
        assert!(status(&picker).contains("not changed"), "{}", status(&picker));
        assert_eq!(picker.data.storage.counts().unwrap().total, 1);
    }

    // ======== Daemon auto-start Tests ========

    #[test]
    fn test_daemon_spawn_args_use_picker_storage_dir() {
        let args = daemon_spawn_args(std::path::Path::new("/custom/clips"));
        let args: Vec<_> = args.iter().map(|a| a.to_str().unwrap()).collect();
        // No --max-entries: the daemon reuses the saved limit
        assert_eq!(args, vec!["--storage-dir", "/custom/clips", "daemon"]);
    }

    #[test]
    fn daemon_spawn_program_is_current_exe() {
        let exe = std::path::PathBuf::from("/opt/clipstack-test/bin/clipstack");
        assert_eq!(daemon_spawn_program(Ok(exe.clone())), Some(exe));

        // The real call site: whatever binary is running (here, the test exe)
        let current = std::env::current_exe().unwrap();
        assert_eq!(daemon_spawn_program(std::env::current_exe()), Some(current));

        // Never fall back to a `clipstack` from PATH
        let err = std::io::Error::other("no /proc");
        assert_eq!(daemon_spawn_program(Err(err)), None);
    }

    // ======== Focus Mode Tests ========

    #[test]
    fn test_focus_toggle_list_to_preview() {
        let (_temp, storage) = create_test_storage(&["test content"]);
        let mut picker = Picker::new(storage).unwrap();

        assert_eq!(picker.view.focus, Focus::List);
        picker.view.focus = Focus::Preview;
        assert_eq!(picker.view.focus, Focus::Preview);
    }

    // ======== Preview Content Loading Tests ========

    #[test]
    fn test_load_preview_content_wraps_lines() {
        let (_temp, storage) = create_test_storage(&[&"x".repeat(200)]);
        let mut picker = Picker::new(storage).unwrap();

        picker.wrap_preview(80);

        // 200 chars / 80 wrap width = 2-3 lines
        assert!(picker.view.preview_lines.len() >= 2, "Long line should be wrapped");
    }

    #[test]
    fn test_load_preview_content_multiline() {
        let (_temp, storage) = create_test_storage(&["line1\nline2\nline3"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.wrap_preview(80);

        let lines = &picker.view.preview_lines;
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().any(|l| l.contains("line1")));
        assert!(lines.iter().any(|l| l.contains("line2")));
        assert!(lines.iter().any(|l| l.contains("line3")));
    }

    #[test]
    fn test_load_preview_content_resets_scroll() {
        let (_temp, storage) = create_test_storage(&["content"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.view.preview_scroll = 50;
        picker.apply(Action::FocusPreview).unwrap();

        assert_eq!(picker.view.preview_scroll, 0);
    }

    // ======== Scroll Calculation Tests ========

    #[test]
    fn test_max_preview_scroll_short_content() {
        let (_temp, storage) = create_test_storage(&["a"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.view.preview_lines = vec!["line1".to_string(), "line2".to_string()];
        picker.view.preview_height = 10;

        // 2 lines < 10 height, no scrolling needed
        assert_eq!(picker.view.max_preview_scroll(), 0);
    }

    #[test]
    fn test_max_preview_scroll_long_content() {
        let (_temp, storage) = create_test_storage(&["a"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.view.preview_lines = (0..100).map(|i| format!("Line {}", i)).collect();
        picker.view.preview_height = 10;

        // 100 lines - 10 height = 90 max scroll
        assert_eq!(picker.view.max_preview_scroll(), 90);
    }

    #[test]
    fn test_max_preview_scroll_exact_fit() {
        let (_temp, storage) = create_test_storage(&["a"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.view.preview_lines = (0..10).map(|i| format!("Line {}", i)).collect();
        picker.view.preview_height = 10;

        assert_eq!(picker.view.max_preview_scroll(), 0);
    }

    // ======== Scroll Bounds Tests ========

    #[test]
    fn test_scroll_saturating_sub_at_zero() {
        let (_temp, storage) = create_test_storage(&["a"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.view.preview_scroll = 0;
        let new_scroll = picker.view.preview_scroll.saturating_sub(1);
        assert_eq!(new_scroll, 0);
    }

    #[test]
    fn test_scroll_respects_max() {
        let (_temp, storage) = create_test_storage(&["a"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.view.preview_lines = (0..100).map(|i| format!("Line {}", i)).collect();
        picker.view.preview_height = 10;
        picker.view.preview_scroll = 90;

        assert!(picker.view.preview_scroll <= picker.view.max_preview_scroll());
    }

    // ======== Line Wrapping Edge Cases ========

    #[test]
    fn test_wrap_unicode_content() {
        let (_temp, storage) = create_test_storage(&["日本語\n中文\n한국어"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.wrap_preview(80);

        assert_eq!(picker.view.preview_lines.len(), 3);
    }

    #[test]
    fn test_wrap_very_long_line() {
        let (_temp, storage) = create_test_storage(&[&"a".repeat(1000)]);
        let mut picker = Picker::new(storage).unwrap();

        picker.wrap_preview(80);

        // 1000 columns / 80 = 13 lines
        assert_eq!(picker.view.preview_lines.len(), 13);
    }

    #[test]
    fn test_wrap_unicode_by_display_width_not_bytes() {
        // 40 CJK chars are 120 bytes but 80 columns: exactly one line at width 80
        let content = "中".repeat(40);
        let (_temp, storage) = create_test_storage(&[&content]);
        let mut picker = Picker::new(storage).unwrap();

        picker.wrap_preview(80);

        assert_eq!(picker.view.preview_lines, [content]);
    }

    #[test]
    fn test_wrap_long_unicode_line() {
        // 100 CJK chars are 200 columns: 40 + 40 + 20 chars at width 80
        let content = "中".repeat(100);
        let (_temp, storage) = create_test_storage(&[&content]);
        let mut picker = Picker::new(storage).unwrap();

        picker.wrap_preview(80);

        let counts: Vec<_> = picker.view.preview_lines.iter().map(|l| l.chars().count()).collect();
        assert_eq!(counts, [40, 40, 20]);
    }

    #[test]
    fn w_and_n_in_preview_rewrap_both_panes() {
        let (_temp, storage) = create_test_storage(&["a b\nc"]);
        let mut picker = Picker::new(storage).unwrap();
        picker.wrap_preview(80);
        assert_eq!(picker.view.preview_lines, ["a b", "c"]);
        press(&mut picker, "\tw");
        picker.wrap_preview(80);
        assert_eq!(picker.view.preview_lines, ["a·b¶", "c"]);
        press(&mut picker, "n");
        picker.wrap_preview(80);
        assert_eq!(picker.view.preview_lines, ["   1 │ a·b¶", "   2 │ c"]);
        // Back in list focus the preview pane keeps the flags
        press(&mut picker, "q");
        assert_eq!(picker.view.focus, Focus::List);
        picker.wrap_preview(80);
        assert_eq!(picker.view.preview_lines, ["   1 │ a·b¶", "   2 │ c"]);
        press(&mut picker, "\twn");
        picker.wrap_preview(80);
        assert_eq!(picker.view.preview_lines, ["a b", "c"]);
    }

    #[test]
    fn preview_rewraps_for_another_entry() {
        let (_temp, storage) = create_test_storage(&["short", &"z".repeat(100)]);
        let mut picker = Picker::new(storage).unwrap();
        picker.wrap_preview(80);
        assert_eq!(picker.view.preview_lines.len(), 2, "newest first: the long entry");
        picker.apply(Action::Move(1)).unwrap();
        picker.wrap_preview(80);
        assert_eq!(picker.view.preview_lines, ["short"]);
    }
}
