//! The history picker (TUI). `data` holds the entries, `state` the view,
//! `search` the filter, `keys` the key handling and `render` the drawing.

mod data;
mod keys;
mod render;
mod search;
mod state;
mod text;

use crate::clipboard::Clipboard;
use crate::config::PickerConfig;
use crate::daemon::Daemon;
use crate::storage::{ClipEntry, IngestOutcome, ListOrder, Source, Storage};
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
use state::{Focus, Mode, StatusLevel, ViewState};
use std::io::{Stdout, stdout};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How the picker ended.
#[derive(Debug, PartialEq)]
enum Outcome {
    /// Esc, q or Ctrl+C
    Cancelled,
    /// Enter: this content goes to the clipboard
    Selected(String),
}

pub struct Picker {
    data: PickerData,
    view: ViewState,
    search: SearchState,
    /// Wait after the last keystroke before searching
    debounce: Duration,
}

impl Picker {
    pub fn new(storage: Storage) -> Result<Self> {
        let mut picker = Self {
            data: PickerData::new(storage)?,
            view: ViewState::default(),
            search: SearchState::default(),
            debounce: search::DEBOUNCE,
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
                // Update local state
                self.data.entries[idx].pinned = is_pinned;
                if !is_pinned {
                    self.data.entries[idx].alias = None;
                }

                // Re-sort: pinned entries first
                self.sort_entries_by_pin();
                if !is_pinned {
                    self.select_index(row);
                    self.load_preview();
                }

                let msg = if is_pinned { "★ Pinned" } else { "Unpinned" };
                self.view.set_status(msg.to_string(), StatusLevel::Success);
            }
            Err(e) => {
                self.view.set_status(e.to_string(), StatusLevel::Warning);
            }
        }
        Ok(())
    }

    /// Sort entries: pinned first, each group kept in recency (index) order.
    /// The selection is an entry id, so it follows its entry.
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
            Some(id) if self.data.cached_content(id).is_none() => match self.data.storage.load_content(id) {
                Ok(content) => self.data.content_cache = Some((id, content)),
                Err(_) => self.data.content_cache = None,
            },
            None => self.data.content_cache = None,
            _ => {}
        }
    }

    /// Load and wrap preview content for Focus::Preview mode
    fn load_preview_content(&mut self) {
        let entry = match self.selected_entry() {
            Some(e) => e.clone(),
            None => return,
        };

        if let Ok(content) = self.data.storage.load_content(entry.id) {
            self.view.preview_lines = text::wrap_chars(&content, text::WRAP_WIDTH);
            self.view.preview_scroll = 0;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let Some(current) = self.selected_index() else {
            return;
        };
        let new_idx = current.saturating_add_signed(delta);

        // Clear preview cache when selection changes
        let before = self.view.selection;
        self.select_index(new_idx);
        if self.view.selection != before {
            self.view.preview_lines.clear();
            self.view.preview_scroll = 0;
        }
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
            let content = self.data.storage.load_content(entry.id)?;
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

    fn undo_delete(&mut self) -> Result<()> {
        if let Some(deleted) = self.data.last_deleted.take() {
            if deleted.deleted_at.elapsed() < UNDO_WINDOW {
                let preview: String = deleted.entry.preview.chars().take(30).collect();
                let restored = match self.data.storage.ingest(&deleted.content, Source::Picker, &[])? {
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
                    let content = self.data.storage.load_content(entry.id)?;
                    return Ok(Some(Outcome::Selected(content)));
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
            Action::Delete => self.delete_selected()?,
            Action::Undo => self.undo_delete()?,
            Action::TogglePin => self.toggle_pin_selected()?,
            Action::FocusPreview => {
                self.load_preview_content();
                self.view.focus = Focus::Preview;
            }
            Action::FocusList => {
                self.view.focus = Focus::List;
                self.view.preview_lines.clear();
                self.view.preview_scroll = 0;
            }
            Action::Scroll(lines) => self.scroll_preview(lines),
            Action::ScrollPage(pages) => {
                self.scroll_preview(pages.saturating_mul(self.view.preview_height as isize));
            }
            Action::ScrollTop => self.view.preview_scroll = 0,
            Action::ScrollBottom => self.view.preview_scroll = self.view.max_preview_scroll(),
        }
        Ok(None)
    }

    /// Scroll the preview by `lines`, within its bounds.
    fn scroll_preview(&mut self, lines: isize) {
        let view = &mut self.view;
        view.preview_scroll = view.preview_scroll.saturating_add_signed(lines).min(view.max_preview_scroll());
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

    fn event_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<Option<String>> {
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

                match self.handle_key(key)? {
                    Some(Outcome::Selected(content)) => return Ok(Some(content)),
                    Some(Outcome::Cancelled) => return Ok(None),
                    None => {}
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

/// Run the picker and paste the selected content to clipboard
pub fn pick_and_paste(storage: Storage, config: &PickerConfig) -> Result<bool> {
    // Ensure daemon is running before showing picker
    ensure_daemon_running(storage.base_dir());

    let mut picker = Picker::new(storage)?;
    picker.debounce = config.search_debounce();

    match picker.run() {
        Ok(Some(content)) => {
            // Content was selected
            Clipboard::system().copy(&content)?;
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
            outcome = picker.handle_key(KeyEvent::new(KeyCode::Char(c), mods)).unwrap();
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

        picker.load_preview_content();

        // 200 chars / 80 wrap width = 2-3 lines
        assert!(picker.view.preview_lines.len() >= 2, "Long line should be wrapped");
    }

    #[test]
    fn test_load_preview_content_multiline() {
        let (_temp, storage) = create_test_storage(&["line1\nline2\nline3"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.load_preview_content();

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
        picker.load_preview_content();

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

        picker.load_preview_content();

        assert_eq!(picker.view.preview_lines.len(), 3);
    }

    #[test]
    fn test_wrap_very_long_line() {
        let (_temp, storage) = create_test_storage(&[&"a".repeat(1000)]);
        let mut picker = Picker::new(storage).unwrap();

        picker.load_preview_content();

        // 1000 chars / 80 = 12-13 lines
        assert!(picker.view.preview_lines.len() >= 12);
    }

    #[test]
    fn test_wrap_unicode_chars_not_bytes() {
        // Test that Unicode lines are wrapped by character count, not byte count
        // 50 Chinese characters = 150 bytes, but should NOT be wrapped at width 80
        let content = "中".repeat(50);
        let (_temp, storage) = create_test_storage(&[&content]);
        let mut picker = Picker::new(storage).unwrap();

        picker.load_preview_content();

        // 50 chars < 80 wrap_width, so should be exactly 1 line
        // Before fix, this would be wrapped because 150 bytes > 80
        assert_eq!(picker.view.preview_lines.len(), 1, "50 Unicode chars should not wrap at width 80");
        assert_eq!(picker.view.preview_lines[0].chars().count(), 50);
    }

    #[test]
    fn test_wrap_long_unicode_line() {
        // 100 Chinese characters should wrap to 2 lines at width 80
        let content = "中".repeat(100);
        let (_temp, storage) = create_test_storage(&[&content]);
        let mut picker = Picker::new(storage).unwrap();

        picker.load_preview_content();

        // 100 chars / 80 = 2 lines (80 + 20)
        assert_eq!(picker.view.preview_lines.len(), 2, "100 Unicode chars should wrap to 2 lines at width 80");
        assert_eq!(picker.view.preview_lines[0].chars().count(), 80);
        assert_eq!(picker.view.preview_lines[1].chars().count(), 20);
    }
}
