//! View state: mode, focus, cursor, query and transient messages. The
//! entries themselves live in `PickerData`.

use ratatui::widgets::{ListState, ScrollbarState};
use std::time::Instant;

/// Picker mode for vim-style navigation
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Normal, // j/k navigation, typing starts search
    Search, // Active search input
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
    pub selected: ListState,
    pub scroll_state: ScrollbarState,
    pub query: String,
    pub status: Option<(String, StatusLevel, Instant)>,
    pub pending_g: bool,        // For gg command
    pub preview_scroll: usize,  // Current scroll offset in preview
    pub preview_lines: Vec<String>, // Cached wrapped lines of preview content
    pub preview_height: u16,    // Available height for preview area
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            mode: Mode::Normal,
            focus: Focus::default(),
            selected: ListState::default(),
            scroll_state: ScrollbarState::default(),
            query: String::new(),
            status: None,
            pending_g: false,
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
    use super::*;

    // ======== Focus Mode Tests ========

    #[test]
    fn test_focus_enum_default() {
        assert_eq!(Focus::default(), Focus::List);
    }
}
