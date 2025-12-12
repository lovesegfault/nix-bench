//! Event handling for TUI

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};

/// Actions that can be triggered by events
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    SelectNext,
    SelectPrevious,
    None,
}

/// Handle a terminal event
pub fn handle_event(event: Event) -> Action {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => handle_key(key),
        _ => Action::None,
    }
}

fn handle_key(key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
        KeyCode::Up | KeyCode::Char('k') => Action::SelectPrevious,
        KeyCode::Down | KeyCode::Char('j') => Action::SelectNext,
        _ => Action::None,
    }
}
