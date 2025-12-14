//! Keyboard input handling for the TUI
//!
//! Extracts keyboard event handling from the main event loop into
//! a separate module for better organization and testability.

use super::app::{App, PanelFocus};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio_util::sync::CancellationToken;

/// Result of handling a keyboard event
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyResult {
    /// Key was handled, continue running
    Handled,
    /// Request cancellation (user confirmed quit)
    Cancel,
    /// Key was not handled (no action taken)
    Unhandled,
}

/// Keyboard event handler
///
/// Handles all keyboard input for the TUI, delegating to mode-specific
/// handlers based on the current app state.
pub struct KeyHandler;

impl KeyHandler {
    /// Handle a keyboard event based on current app state
    ///
    /// Returns `KeyResult` indicating what action to take.
    pub fn handle(app: &mut App, key: KeyEvent, cancel: &CancellationToken) -> KeyResult {
        // Quit confirmation takes priority
        if app.ui.show_quit_confirm {
            return Self::handle_quit_confirm(app, key, cancel);
        }

        // Fullscreen logs mode
        if app.ui.logs_fullscreen {
            return Self::handle_fullscreen_logs(app, key);
        }

        // Normal mode
        Self::handle_normal(app, key)
    }

    /// Handle input in quit confirmation dialog
    fn handle_quit_confirm(
        app: &mut App,
        key: KeyEvent,
        cancel: &CancellationToken,
    ) -> KeyResult {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                app.ui.show_quit_confirm = false;
                cancel.cancel();
                KeyResult::Cancel
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.ui.show_quit_confirm = false;
                KeyResult::Handled
            }
            _ => KeyResult::Handled,
        }
    }

    /// Handle input in fullscreen logs mode
    fn handle_fullscreen_logs(app: &mut App, key: KeyEvent) -> KeyResult {
        use tui_logger::TuiWidgetEvent;

        match key.code {
            KeyCode::Char('q') => {
                app.ui.show_quit_confirm = true;
            }
            KeyCode::Esc => {
                // Esc exits page mode in TuiLoggerSmartWidget, or exits fullscreen
                app.scroll.tracing_log_state.transition(TuiWidgetEvent::EscapeKey);
            }
            KeyCode::Char('l') => {
                app.ui.logs_fullscreen = false;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.scroll.tracing_log_state.transition(TuiWidgetEvent::UpKey);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.scroll.tracing_log_state.transition(TuiWidgetEvent::DownKey);
            }
            KeyCode::Left | KeyCode::Char('h') => {
                app.scroll.tracing_log_state.transition(TuiWidgetEvent::LeftKey);
            }
            KeyCode::Right => {
                app.scroll.tracing_log_state.transition(TuiWidgetEvent::RightKey);
            }
            KeyCode::PageUp => {
                app.scroll
                    .tracing_log_state
                    .transition(TuiWidgetEvent::PrevPageKey);
            }
            KeyCode::PageDown => {
                app.scroll
                    .tracing_log_state
                    .transition(TuiWidgetEvent::NextPageKey);
            }
            KeyCode::Char(' ') => {
                // Space toggles page mode / hides filtered targets
                app.scroll.tracing_log_state.transition(TuiWidgetEvent::SpaceKey);
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                // Increase log level for selected target
                app.scroll.tracing_log_state.transition(TuiWidgetEvent::PlusKey);
            }
            KeyCode::Char('-') => {
                // Decrease log level for selected target
                app.scroll.tracing_log_state.transition(TuiWidgetEvent::MinusKey);
            }
            KeyCode::Char('f') => {
                // Focus on selected target
                app.scroll.tracing_log_state.transition(TuiWidgetEvent::FocusKey);
            }
            KeyCode::Tab => {
                // Hide/show target selector panel
                app.scroll.tracing_log_state.transition(TuiWidgetEvent::HideKey);
            }
            _ => return KeyResult::Unhandled,
        }
        KeyResult::Handled
    }

    /// Handle input in normal mode
    fn handle_normal(app: &mut App, key: KeyEvent) -> KeyResult {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                app.ui.show_quit_confirm = true;
            }
            KeyCode::Tab => {
                app.toggle_focus();
            }
            KeyCode::Up | KeyCode::Char('k') => match app.ui.focus {
                PanelFocus::InstanceList => app.select_previous(),
                PanelFocus::BuildOutput => app.scroll_up(1),
            },
            KeyCode::Down | KeyCode::Char('j') => match app.ui.focus {
                PanelFocus::InstanceList => app.select_next(),
                PanelFocus::BuildOutput => app.scroll_down(1),
            },
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if app.ui.focus == PanelFocus::BuildOutput {
                    app.scroll_down(10);
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if app.ui.focus == PanelFocus::BuildOutput {
                    app.scroll_up(10);
                }
            }
            KeyCode::Char('g') => {
                if app.ui.focus == PanelFocus::BuildOutput {
                    app.scroll_to_top();
                }
            }
            KeyCode::Char('G') => {
                if app.ui.focus == PanelFocus::BuildOutput {
                    app.scroll_to_bottom();
                }
            }
            KeyCode::Char('f') => {
                if app.ui.focus == PanelFocus::BuildOutput {
                    app.scroll.log_auto_follow = !app.scroll.log_auto_follow;
                }
            }
            KeyCode::Char('l') => {
                app.ui.logs_fullscreen = !app.ui.logs_fullscreen;
            }
            KeyCode::Char('?') | KeyCode::F(1) => {
                app.toggle_help();
            }
            KeyCode::Home => match app.ui.focus {
                PanelFocus::InstanceList => app.instances.selected_index = 0,
                PanelFocus::BuildOutput => app.scroll_to_top(),
            },
            KeyCode::End => match app.ui.focus {
                PanelFocus::InstanceList => {
                    app.instances.selected_index =
                        app.instances.order.len().saturating_sub(1);
                }
                PanelFocus::BuildOutput => app.scroll_to_bottom(),
            },
            _ => return KeyResult::Unhandled,
        }
        KeyResult::Handled
    }
}
