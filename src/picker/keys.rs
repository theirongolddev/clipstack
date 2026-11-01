//! Key bindings: `map_key` turns a key press into an `Action` from the view
//! state alone (pure). `/` starts a search; in normal mode bare letters are
//! commands (vim-like), so typing a word there never filters, and a single
//! `d` only arms delete (`dd` deletes).

use super::state::{Focus, Mode, ViewState};
#[cfg(test)]
use super::state::Prompt;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What a key press asks the picker to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    /// Copy the selected entry and exit
    Select,
    /// Move the list selection by this many rows
    Move(isize),
    JumpTop,
    JumpBottom,
    /// First key of `gg` or `dd`
    Pending(char),
    EnterSearch,
    ExitSearch,
    SearchInput(char),
    SearchBackspace,
    Delete,
    Undo,
    TogglePin,
    FocusPreview,
    FocusList,
    /// Scroll the preview by this many lines
    Scroll(isize),
    /// Scroll the preview by this many pages
    ScrollPage(isize),
    ScrollTop,
    ScrollBottom,
    ToggleWhitespace,
    ToggleLineNumbers,
    OpenAliasPrompt,
    PromptInput(char),
    PromptBackspace,
    PromptSubmit,
    PromptCancel,
}

/// The action for `key` in the current mode and focus.
pub fn map_key(view: &ViewState, key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Action::Quit;
    }
    if view.prompt.is_some() {
        return prompt_key(key);
    }
    match (view.mode, view.focus) {
        (Mode::Search, _) => search_key(key),
        (Mode::Normal, Focus::Preview) => preview_key(key),
        (Mode::Normal, Focus::List) => list_key(view.pending, key),
    }
}

/// The character of a key pressed without Ctrl or Alt (Shift is allowed:
/// `G` arrives as Shift+G).
fn plain(key: KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => Some(c),
        _ => None,
    }
}

fn ctrl(key: KeyEvent, c: char) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char(c)
}

/// Normal mode, list focus: vim-like commands; letters never filter.
fn list_key(pending: Option<char>, key: KeyEvent) -> Action {
    let c = plain(key);
    match (pending, c) {
        (Some('g'), Some('g')) => return Action::JumpTop,
        (Some('d'), Some('d')) => return Action::Delete,
        _ => {}
    }
    if ctrl(key, 'd') {
        return Action::Move(10);
    }
    if ctrl(key, 'u') {
        return Action::Move(-10);
    }
    match (key.code, c) {
        (KeyCode::Esc, _) | (_, Some('q')) => Action::Quit,
        (KeyCode::Enter, _) => Action::Select,
        (KeyCode::Down, _) | (_, Some('j')) => Action::Move(1),
        (KeyCode::Up, _) | (_, Some('k')) => Action::Move(-1),
        (KeyCode::PageDown, _) => Action::Move(10),
        (KeyCode::PageUp, _) => Action::Move(-10),
        (KeyCode::Tab, _) => Action::FocusPreview,
        (_, Some('G')) => Action::JumpBottom,
        (_, Some(p @ ('g' | 'd'))) => Action::Pending(p),
        (_, Some('/')) => Action::EnterSearch,
        (_, Some('p')) => Action::TogglePin,
        (_, Some('P')) => Action::OpenAliasPrompt,
        (_, Some('u')) => Action::Undo,
        _ => Action::None,
    }
}

/// Search mode: characters edit the query.
fn search_key(key: KeyEvent) -> Action {
    if ctrl(key, 'n') {
        return Action::Move(1);
    }
    if ctrl(key, 'p') {
        return Action::Move(-1);
    }
    match key.code {
        KeyCode::Esc => Action::ExitSearch,
        KeyCode::Enter => Action::Select,
        KeyCode::Down => Action::Move(1),
        KeyCode::Up => Action::Move(-1),
        KeyCode::Backspace => Action::SearchBackspace,
        _ => plain(key).map_or(Action::None, Action::SearchInput),
    }
}

/// Normal mode, preview focus: scrolling.
/// The alias prompt: every printable key is text.
fn prompt_key(key: KeyEvent) -> Action {
    match (key.code, plain(key)) {
        (KeyCode::Enter, _) => Action::PromptSubmit,
        (KeyCode::Esc, _) => Action::PromptCancel,
        (KeyCode::Backspace, _) => Action::PromptBackspace,
        (KeyCode::Char(_), Some(c)) => Action::PromptInput(c),
        _ => Action::None,
    }
}

fn preview_key(key: KeyEvent) -> Action {
    match (key.code, plain(key)) {
        (KeyCode::Tab | KeyCode::Esc, _) | (_, Some('q')) => Action::FocusList,
        (KeyCode::Down, _) | (_, Some('j')) => Action::Scroll(1),
        (KeyCode::Up, _) | (_, Some('k')) => Action::Scroll(-1),
        (KeyCode::PageDown, _) => Action::ScrollPage(1),
        (KeyCode::PageUp, _) => Action::ScrollPage(-1),
        (KeyCode::Home, _) | (_, Some('g')) => Action::ScrollTop,
        (KeyCode::End, _) | (_, Some('G')) => Action::ScrollBottom,
        (_, Some('w')) => Action::ToggleWhitespace,
        (_, Some('n')) => Action::ToggleLineNumbers,
        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        let mods = if c.is_ascii_uppercase() { KeyModifiers::SHIFT } else { KeyModifiers::NONE };
        KeyEvent::new(KeyCode::Char(c), mods)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn view(mode: Mode, focus: Focus) -> ViewState {
        ViewState { mode, focus, ..ViewState::default() }
    }

    /// Feed `keys` through `map_key`, keeping `pending` as the picker does.
    fn type_keys(mut state: ViewState, keys: &str) -> Vec<Action> {
        keys.chars()
            .map(|c| {
                let action = map_key(&state, ch(c));
                state.pending = match action {
                    Action::Pending(p) => Some(p),
                    _ => None,
                };
                action
            })
            .collect()
    }

    #[test]
    fn j_moves_down_in_list_focus() {
        let normal = view(Mode::Normal, Focus::List);
        assert_eq!(map_key(&normal, ch('j')), Action::Move(1));
        assert_eq!(map_key(&normal, key(KeyCode::Down)), Action::Move(1));
        assert_eq!(map_key(&normal, ch('k')), Action::Move(-1));
        assert_eq!(map_key(&normal, key(KeyCode::Up)), Action::Move(-1));
        assert_eq!(map_key(&normal, ctrl('d')), Action::Move(10));
        assert_eq!(map_key(&normal, ctrl('u')), Action::Move(-10));
        assert_eq!(map_key(&normal, key(KeyCode::PageDown)), Action::Move(10));
        assert_eq!(map_key(&normal, key(KeyCode::PageUp)), Action::Move(-10));
        assert_eq!(map_key(&normal, ch('G')), Action::JumpBottom);
        assert_eq!(type_keys(view(Mode::Normal, Focus::List), "gg"), [Action::Pending('g'), Action::JumpTop]);
        assert_eq!(map_key(&normal, key(KeyCode::Tab)), Action::FocusPreview);
        assert_eq!(map_key(&normal, key(KeyCode::Enter)), Action::Select);
        // In search mode j is text, not a move
        assert_eq!(map_key(&view(Mode::Search, Focus::List), ch('j')), Action::SearchInput('j'));
    }

    #[test]
    fn w_and_n_only_bound_in_preview() {
        let preview = view(Mode::Normal, Focus::Preview);
        assert_eq!(map_key(&preview, ch('w')), Action::ToggleWhitespace);
        assert_eq!(map_key(&preview, ch('n')), Action::ToggleLineNumbers);
        assert_eq!(map_key(&view(Mode::Normal, Focus::List), ch('w')), Action::None);
        assert_eq!(map_key(&view(Mode::Normal, Focus::List), ch('n')), Action::None);
        assert_eq!(map_key(&view(Mode::Search, Focus::List), ch('w')), Action::SearchInput('w'));
        assert_eq!(map_key(&view(Mode::Search, Focus::List), ch('n')), Action::SearchInput('n'));
        assert_eq!(map_key(&preview, ctrl('w')), Action::None);
    }

    #[test]
    fn shift_p_opens_alias_prompt_and_prompt_keys_are_text() {
        assert_eq!(map_key(&view(Mode::Normal, Focus::List), ch('P')), Action::OpenAliasPrompt);
        assert_eq!(map_key(&view(Mode::Normal, Focus::Preview), ch('P')), Action::None);
        let mut prompt = view(Mode::Normal, Focus::List);
        prompt.prompt = Some(Prompt { id: 1, input: String::new() });
        for c in "qdPpj@-_".chars() {
            assert_eq!(map_key(&prompt, ch(c)), Action::PromptInput(c), "{c:?}");
        }
        assert_eq!(map_key(&prompt, key(KeyCode::Enter)), Action::PromptSubmit);
        assert_eq!(map_key(&prompt, key(KeyCode::Esc)), Action::PromptCancel);
        assert_eq!(map_key(&prompt, key(KeyCode::Backspace)), Action::PromptBackspace);
        assert_eq!(map_key(&prompt, key(KeyCode::Tab)), Action::None);
        assert_eq!(map_key(&prompt, ctrl('c')), Action::Quit);
    }

    #[test]
    fn slash_enters_search_and_letters_are_commands() {
        let normal = view(Mode::Normal, Focus::List);
        assert_eq!(map_key(&normal, ch('/')), Action::EnterSearch);
        assert_eq!(map_key(&normal, ch('p')), Action::TogglePin);
        assert_eq!(map_key(&normal, ch('u')), Action::Undo);
        assert_eq!(map_key(&normal, ch('q')), Action::Quit);
        assert_eq!(map_key(&normal, key(KeyCode::Esc)), Action::Quit);
        // Letters without a binding do nothing: they never start a search
        for c in "abcfhilmorstvxyz0123456789 ".chars() {
            assert_eq!(map_key(&normal, ch(c)), Action::None, "{c:?}");
        }
        // A control chord is not its bare letter (Ctrl+P must not pin)
        assert_eq!(map_key(&normal, ctrl('p')), Action::None);

        let search = view(Mode::Search, Focus::List);
        for c in "dpuqg/ G".chars() {
            assert_eq!(map_key(&search, ch(c)), Action::SearchInput(c), "{c:?}");
        }
        assert_eq!(map_key(&search, key(KeyCode::Esc)), Action::ExitSearch);
        assert_eq!(map_key(&search, key(KeyCode::Backspace)), Action::SearchBackspace);
        assert_eq!(map_key(&search, key(KeyCode::Enter)), Action::Select);
        assert_eq!(map_key(&search, ctrl('n')), Action::Move(1));
        assert_eq!(map_key(&search, ctrl('p')), Action::Move(-1));
        assert_eq!(map_key(&search, key(KeyCode::Down)), Action::Move(1));
        assert_eq!(map_key(&search, key(KeyCode::Up)), Action::Move(-1));
        // Ctrl+C quits from every mode and focus
        for state in [&normal, &search, &view(Mode::Normal, Focus::Preview)] {
            assert_eq!(map_key(state, ctrl('c')), Action::Quit);
        }
    }

    #[test]
    fn typing_docker_in_normal_mode_deletes_nothing() {
        let actions = type_keys(view(Mode::Normal, Focus::List), "docker");
        assert!(!actions.contains(&Action::Delete), "{actions:?}");
        assert!(!actions.iter().any(|a| matches!(a, Action::SearchInput(_) | Action::EnterSearch)), "{actions:?}");
        // dd deletes; a d followed by anything else is dropped
        assert_eq!(type_keys(view(Mode::Normal, Focus::List), "dd"), [Action::Pending('d'), Action::Delete]);
        assert_eq!(type_keys(view(Mode::Normal, Focus::List), "dgd"), [
            Action::Pending('d'),
            Action::Pending('g'),
            Action::Pending('d')
        ]);
        // Typed after /, "docker" is a query
        let typed = type_keys(view(Mode::Search, Focus::List), "docker");
        assert_eq!(typed, "docker".chars().map(Action::SearchInput).collect::<Vec<_>>());
    }

    #[test]
    fn q_in_preview_returns_to_list() {
        let preview = view(Mode::Normal, Focus::Preview);
        assert_eq!(map_key(&preview, ch('q')), Action::FocusList);
        assert_eq!(map_key(&preview, key(KeyCode::Esc)), Action::FocusList);
        assert_eq!(map_key(&preview, key(KeyCode::Tab)), Action::FocusList);
        assert_eq!(map_key(&preview, ch('j')), Action::Scroll(1));
        assert_eq!(map_key(&preview, ch('k')), Action::Scroll(-1));
        assert_eq!(map_key(&preview, key(KeyCode::PageDown)), Action::ScrollPage(1));
        assert_eq!(map_key(&preview, key(KeyCode::PageUp)), Action::ScrollPage(-1));
        assert_eq!(map_key(&preview, ch('g')), Action::ScrollTop);
        assert_eq!(map_key(&preview, key(KeyCode::Home)), Action::ScrollTop);
        assert_eq!(map_key(&preview, ch('G')), Action::ScrollBottom);
        assert_eq!(map_key(&preview, key(KeyCode::End)), Action::ScrollBottom);
        // No list commands while the preview has focus
        assert_eq!(map_key(&preview, ch('d')), Action::None);
        assert_eq!(map_key(&preview, ch('p')), Action::None);
    }
}
