//! View state: mode, focus, cursor, query and transient messages. The
//! entries themselves live in `PickerData`.

use crate::storage::EntryId;
use std::time::Instant;

/// Picker mode for vim-style navigation
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Normal, // Bare letters are commands (j/k, dd, p ...); `/` starts a search
    Search, // Typing edits the query; Esc returns to Normal
}

/// Focus mode for preview scrolling
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub enum Focus {
    #[default]
    List, // Normal mode - navigate entry list
    Preview, // Preview mode - scroll through selected entry content
}

/// Status message level for toast-like feedback
#[derive(Clone, Copy)]
pub enum StatusLevel {
    Success,
    Warning,
}

pub struct ViewState {
    pub mode: Mode,
    pub focus: Focus,
    /// The selected entry (by id, so it survives re-sorting and filtering)
    pub selection: Option<EntryId>,
    /// First visible list row (kept between frames)
    pub list_offset: usize,
    pub query: String,
    pub status: Option<(String, StatusLevel, Instant)>,
    /// First key of a two-key command (`gg`, `dd`)
    pub pending: Option<char>,
    pub preview_scroll: usize,  // Current scroll offset in preview
    pub preview_lines: Vec<String>, // Cached wrapped lines of preview content
    pub preview_height: u16,    // Available height for preview area
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            mode: Mode::Normal,
            focus: Focus::default(),
            selection: None,
            list_offset: 0,
            query: String::new(),
            status: None,
            pending: None,
            preview_scroll: 0,
            preview_lines: Vec::new(),
            preview_height: 10, // Updated dynamically during render
        }
    }
}

impl ViewState {
    pub fn set_status(&mut self, msg: String, level: StatusLevel) {
        self.status = Some((msg, level, Instant::now()));
    }

    /// Calculate max scroll offset for preview mode
    pub fn max_preview_scroll(&self) -> usize {
        self.preview_lines
            .len()
            .saturating_sub(self.preview_height as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::super::Picker;
    use super::super::tests::create_test_storage;
    use super::*;
    use crate::storage::ListOrder;

    fn selected_preview(picker: &Picker) -> Option<&str> {
        picker.selected_entry().map(|e| e.preview.as_str())
    }

    #[test]
    fn selection_survives_unpin() {
        let (_temp, storage) = create_test_storage(&["a", "b", "c", "d", "e"]);
        for e in storage.list(ListOrder::Recency, None).unwrap() {
            if ["a", "b", "c"].contains(&e.preview.as_str()) {
                storage.toggle_pin(e.id).unwrap();
            }
        }
        let mut picker = Picker::new(storage).unwrap();
        // Pinned (most recent first), then the rest: c b a e d
        picker.apply(super::super::keys::Action::Move(1)).unwrap();
        assert_eq!(selected_preview(&picker), Some("b"));
        let id_b = picker.view.selection;

        // Unpin b: it moves below the pins; the cursor stays on row 1 (FEATURE_PLAN Feature 4)
        picker.apply(super::super::keys::Action::TogglePin).unwrap();
        assert_eq!(picker.selected_index(), Some(1));
        assert_eq!(selected_preview(&picker), Some("a"));
        assert_ne!(picker.view.selection, id_b);
        assert!(picker.view.selection.is_some());

        // Pin e (row 2 now: c a | e d b): the cursor follows it to the pins
        picker.apply(super::super::keys::Action::Move(1)).unwrap();
        assert_eq!(selected_preview(&picker), Some("e"));
        picker.apply(super::super::keys::Action::TogglePin).unwrap();
        assert_eq!(selected_preview(&picker), Some("e"));
        assert_eq!(picker.selected_index(), Some(0));
    }

    #[test]
    fn selection_follows_entry_when_filter_keeps_it() {
        let (_temp, storage) = create_test_storage(&["apple", "banana", "apricot"]);
        let mut picker = Picker::new(storage).unwrap();
        // apricot banana apple
        picker.apply(super::super::keys::Action::Move(2)).unwrap();
        assert_eq!(selected_preview(&picker), Some("apple"));
        picker.view.query = "ap".to_string();
        picker.update_filter();
        assert_eq!(selected_preview(&picker), Some("apple"), "still visible, still selected");
        picker.view.query = "ban".to_string();
        picker.update_filter();
        assert_eq!(selected_preview(&picker), Some("banana"), "hidden: the first match is selected");
    }

    #[test]
    fn selection_none_when_filter_empty() {
        let (_temp, storage) = create_test_storage(&["apple", "banana"]);
        let mut picker = Picker::new(storage).unwrap();
        assert!(picker.view.selection.is_some());
        picker.view.query = "zzz_nothing".to_string();
        picker.update_filter();
        assert_eq!(picker.view.selection, None);
        assert_eq!(picker.selected_index(), None);
        assert!(picker.selected_entry().is_none());
        picker.view.query.clear();
        picker.update_filter();
        assert_eq!(selected_preview(&picker), Some("banana"));
    }

    #[test]
    fn delete_selects_the_next_row() {
        let (_temp, storage) = create_test_storage(&["a", "b", "c"]);
        let mut picker = Picker::new(storage).unwrap();
        // c b a: delete b, land on a; delete a (last row), land on c
        picker.apply(super::super::keys::Action::Move(1)).unwrap();
        picker.apply(super::super::keys::Action::Delete).unwrap();
        assert_eq!(selected_preview(&picker), Some("a"));
        picker.apply(super::super::keys::Action::Delete).unwrap();
        assert_eq!(selected_preview(&picker), Some("c"));
        picker.apply(super::super::keys::Action::Delete).unwrap();
        assert_eq!(picker.view.selection, None);
    }

    // ======== Focus Mode Tests ========

    #[test]
    fn test_focus_enum_default() {
        assert_eq!(Focus::default(), Focus::List);
    }
}
