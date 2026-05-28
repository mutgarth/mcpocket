use crossterm::event::KeyCode;

/// A high-level UI action decoded from a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    NextTab,
    PrevTab,
    Up,
    Down,
    Enable,
    Disable,
    Allow,
    Deny,
    Refresh,
    None,
}

/// Map a key code to an action. Tab-specific interpretation (e.g. Enable only
/// applies on the Servers tab) is handled by the caller.
pub fn map_key(code: KeyCode) -> Action {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
        KeyCode::Tab => Action::NextTab,
        KeyCode::BackTab => Action::PrevTab,
        KeyCode::Down | KeyCode::Char('j') => Action::Down,
        KeyCode::Up | KeyCode::Char('k') => Action::Up,
        KeyCode::Char('e') => Action::Enable,
        KeyCode::Char('d') => Action::Disable,
        KeyCode::Char('a') => Action::Allow,
        KeyCode::Char('x') => Action::Deny,
        KeyCode::Char('r') => Action::Refresh,
        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    #[test]
    fn quit_keys() {
        assert_eq!(map_key(KeyCode::Char('q')), Action::Quit);
        assert_eq!(map_key(KeyCode::Esc), Action::Quit);
    }

    #[test]
    fn navigation_keys() {
        assert_eq!(map_key(KeyCode::Tab), Action::NextTab);
        assert_eq!(map_key(KeyCode::BackTab), Action::PrevTab);
        assert_eq!(map_key(KeyCode::Down), Action::Down);
        assert_eq!(map_key(KeyCode::Char('j')), Action::Down);
        assert_eq!(map_key(KeyCode::Up), Action::Up);
        assert_eq!(map_key(KeyCode::Char('k')), Action::Up);
    }

    #[test]
    fn edit_keys() {
        assert_eq!(map_key(KeyCode::Char('e')), Action::Enable);
        assert_eq!(map_key(KeyCode::Char('d')), Action::Disable);
        assert_eq!(map_key(KeyCode::Char('a')), Action::Allow);
        assert_eq!(map_key(KeyCode::Char('x')), Action::Deny);
        assert_eq!(map_key(KeyCode::Char('r')), Action::Refresh);
    }

    #[test]
    fn unknown_key_is_none() {
        assert_eq!(map_key(KeyCode::Char('z')), Action::None);
    }
}
