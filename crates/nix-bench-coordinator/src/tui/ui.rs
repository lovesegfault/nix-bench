//! UI rendering

use crate::orchestrator::InstanceStatus;
use crate::tui::app::App;
use crate::tui::theme;
use crate::tui::widgets::{aggregate_stats, instance_detail, instance_list};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use tui_logger::{TuiLoggerSmartWidget, TuiLoggerWidget};

/// Render the entire UI
pub fn render(frame: &mut Frame, app: &mut App) {
    // Check if fullscreen logs mode is active
    if app.ui.logs_fullscreen {
        render_fullscreen_logs(frame, app);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Header/title bar
            Constraint::Min(10),   // Main content (now includes stats panel)
            Constraint::Length(8), // Tracing logs
            Constraint::Length(1), // Help bar
        ])
        .split(frame.area());

    // Render header with elapsed time
    render_header(frame, chunks[0], app);

    // Main content: instances list, detail, and stats panel
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(30),        // Instance list
            Constraint::Percentage(55), // Instance detail
            Constraint::Min(25),        // Stats panel (vertical)
        ])
        .split(chunks[1]);

    // Store the instance list area for mouse click handling
    app.ui.instance_list_area = Some(main_chunks[0]);

    // Render instance list and capture scroll offset for mouse click handling
    app.ui.list_scroll_offset = instance_list::render(frame, main_chunks[0], app);

    // Render instance detail (now includes build output logs)
    if let Some(instance_key) = app.instances.order.get(app.instances.selected_index).cloned() {
        // Ensure scroll state exists for this instance
        app.ensure_scroll_state(&instance_key);

        if let Some(instance) = app.instances.data.get(&instance_key) {
            let scroll_state = app.scroll.log_scroll_states.get_mut(&instance_key).unwrap();
            let logs_area = instance_detail::render(
                frame,
                main_chunks[1],
                instance,
                app.context.total_runs,
                scroll_state,
                app.scroll.log_auto_follow,
                app.ui.focus,
            );
            app.ui.build_output_area = Some(logs_area);
        }
    } else {
        // Clear stale build output area when no instance is selected
        app.ui.build_output_area = None;
        let block = Block::default().title(" Details ").borders(Borders::ALL);
        let paragraph = Paragraph::new("No instance selected")
            .block(block)
            .alignment(Alignment::Center);
        frame.render_widget(paragraph, main_chunks[1]);
    }

    // Render aggregate stats (now vertical panel)
    aggregate_stats::render(frame, main_chunks[2], app);

    // Render tracing logs (auto-scrolls to latest)
    render_tracing_logs(frame, chunks[2]);

    // Render help bar
    render_help_bar(frame, chunks[3]);

    // Render help popup if toggled
    if app.ui.show_help {
        render_help_popup(frame);
    }

    // Render quit confirmation popup if toggled (but not during cleanup)
    if app.ui.show_quit_confirm && !app.is_cleaning_up() {
        render_quit_confirm_popup(frame);
    }
}

/// Render the header bar with title and elapsed time
fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    use crate::tui::app::InitPhase;

    let t = theme::theme();
    let elapsed = app.elapsed_str();

    // Format account ID display (show last 4 digits if available)
    let account_display = app
        .context
        .aws_account_id
        .as_ref()
        .map(|id: &String| format!("AWS:…{}", &id[id.len().saturating_sub(4)..]))
        .unwrap_or_default();

    let header_text = if app.is_initializing() {
        if account_display.is_empty() {
            format!(
                " nix-bench │ {} │ Elapsed: {} ",
                app.lifecycle.init_phase.message(),
                elapsed
            )
        } else {
            format!(
                " nix-bench │ {} │ {} │ Elapsed: {} ",
                account_display,
                app.lifecycle.init_phase.message(),
                elapsed
            )
        }
    } else if let InitPhase::CleaningUp(ref progress) = app.lifecycle.init_phase {
        // Show cleanup progress with percentage
        format!(
            " nix-bench │ {} │ {} │ {:.0}% │ Elapsed: {} ",
            account_display,
            progress.current_step,
            progress.percentage(),
            elapsed
        )
    } else {
        let completion = app.completion_percentage();
        let remaining = app.estimated_remaining_str();
        format!(
            " nix-bench │ {} │ Elapsed: {} │ Progress: {:.1}% │ ETA: {} ",
            account_display, elapsed, completion, remaining
        )
    };

    let is_complete = matches!(&app.lifecycle.init_phase, InitPhase::Completed)
        || (matches!(&app.lifecycle.init_phase, InitPhase::Running) && app.all_complete());
    let is_failed = matches!(&app.lifecycle.init_phase, InitPhase::Failed(_));
    let is_cleanup = matches!(&app.lifecycle.init_phase, InitPhase::CleaningUp(_));

    let style = t.header_style(is_complete, is_failed, is_cleanup);

    let header = Paragraph::new(header_text).style(style);
    frame.render_widget(header, area);
}

/// Render tracing logs widget with colorized log levels
fn render_tracing_logs(frame: &mut Frame, area: Rect) {
    use tui_logger::TuiLoggerLevelOutput;

    let t = theme::theme();

    let widget = TuiLoggerWidget::default()
        .block(
            Block::default()
                .title(" Logs ")
                .borders(Borders::ALL)
                .border_style(t.block_unfocused()),
        )
        .style(Style::default().fg(t.fg))
        .output_level(Some(TuiLoggerLevelOutput::Long))
        .style_error(Style::default().fg(t.log_error))
        .style_warn(Style::default().fg(t.log_warn))
        .style_info(Style::default().fg(t.log_info))
        .style_debug(Style::default().fg(t.log_debug))
        .style_trace(Style::default().fg(t.log_trace));

    frame.render_widget(widget, area);
}

/// Render fullscreen tracing logs view
fn render_fullscreen_logs(frame: &mut Frame, app: &mut App) {
    let t = theme::theme();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Header
            Constraint::Min(1),    // Logs take all space
            Constraint::Length(1), // Help bar
        ])
        .split(frame.area());

    // Header with app state
    let header_text = format!(
        " nix-bench │ Logs (fullscreen) │ Press 'l' to return │ Elapsed: {} ",
        app.elapsed_str()
    );
    let header = Paragraph::new(header_text).style(Style::default().fg(t.fg).bg(t.accent_primary));
    frame.render_widget(header, chunks[0]);

    // Fullscreen colorized tracing logs with scroll state (using SmartWidget for scrolling)
    use tui_logger::TuiLoggerLevelOutput;

    let widget = TuiLoggerSmartWidget::default()
        .style(Style::default().fg(t.fg).bg(t.bg))
        .output_level(Some(TuiLoggerLevelOutput::Long))
        .style_error(Style::default().fg(t.log_error))
        .style_warn(Style::default().fg(t.log_warn))
        .style_info(Style::default().fg(t.log_info))
        .style_debug(Style::default().fg(t.log_debug))
        .style_trace(Style::default().fg(t.log_trace))
        .state(&app.scroll.tracing_log_state);
    frame.render_widget(widget, chunks[1]);

    // Help bar with SmartWidget controls
    let key_style = t.key_badge();
    let help_text = Line::from(vec![
        Span::styled(" ↑↓←→ ", key_style),
        Span::styled(" Navigate ", t.dim()),
        Span::styled(" Tab ", key_style),
        Span::styled(" Toggle panel ", t.dim()),
        Span::styled(" +/- ", key_style),
        Span::styled(" Level ", t.dim()),
        Span::styled(" l ", key_style),
        Span::styled(" Return ", t.dim()),
        Span::styled(" q ", key_style),
        Span::styled(" Quit ", t.dim()),
    ]);
    let help_bar = Paragraph::new(help_text);
    frame.render_widget(help_bar, chunks[2]);

    // Render quit confirmation popup if toggled (but not during cleanup)
    if app.ui.show_quit_confirm && !app.is_cleaning_up() {
        render_quit_confirm_popup(frame);
    }
}

/// Render the help bar at the bottom
fn render_help_bar(frame: &mut Frame, area: Rect) {
    let t = theme::theme();
    let key_style = t.key_badge();

    let help_text = Line::from(vec![
        Span::styled(" Tab ", key_style),
        Span::styled(" Focus ", t.dim()),
        Span::styled(" ↑/k ", key_style),
        Span::styled(" Up ", t.dim()),
        Span::styled(" ↓/j ", key_style),
        Span::styled(" Down ", t.dim()),
        Span::styled(" l ", key_style),
        Span::styled(" Logs ", t.dim()),
        Span::styled(" ? ", key_style),
        Span::styled(" Help ", t.dim()),
        Span::styled(" q ", key_style),
        Span::styled(" Quit ", t.dim()),
    ]);

    let help_bar = Paragraph::new(help_text);
    frame.render_widget(help_bar, area);
}

/// Render help popup overlay
fn render_help_popup(frame: &mut Frame) {
    let t = theme::theme();
    let area = centered_rect(50, 60, frame.area());

    // Clear the area behind the popup
    frame.render_widget(Clear, area);

    let section_style = t.bold();

    let help_text = vec![
        Line::from(""),
        Line::from(vec![Span::styled("  Navigation", section_style)]),
        Line::from(""),
        Line::from(vec![Span::styled("  Tab          Switch focus (list/logs)", t.text())]),
        Line::from(vec![Span::styled("  ↑ / k        Up (select / scroll)", t.text())]),
        Line::from(vec![Span::styled("  ↓ / j        Down (select / scroll)", t.text())]),
        Line::from(vec![Span::styled("  Home         First instance / top", t.text())]),
        Line::from(vec![Span::styled("  End          Last instance / bottom", t.text())]),
        Line::from(""),
        Line::from(vec![Span::styled("  Log Scrolling (when focused)", section_style)]),
        Line::from(""),
        Line::from(vec![Span::styled("  Ctrl+d       Page down", t.text())]),
        Line::from(vec![Span::styled("  Ctrl+u       Page up", t.text())]),
        Line::from(vec![Span::styled("  g            Jump to top", t.text())]),
        Line::from(vec![Span::styled("  G            Jump to bottom (auto-follow)", t.text())]),
        Line::from(vec![Span::styled("  l            Toggle fullscreen logs", t.text())]),
        Line::from(""),
        Line::from(vec![Span::styled("  General", section_style)]),
        Line::from(""),
        Line::from(vec![Span::styled("  ? / F1       Toggle this help", t.text())]),
        Line::from(vec![Span::styled("  q / Esc      Quit (cleanup resources)", t.text())]),
        Line::from(""),
        Line::from(vec![Span::styled("  Mouse", section_style)]),
        Line::from(""),
        Line::from(vec![Span::styled("  Click        Select / focus panel", t.text())]),
        Line::from(vec![Span::styled("  Scroll       Navigate (context-aware)", t.text())]),
        Line::from(""),
        Line::from(vec![Span::styled("  Status Icons", section_style)]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  ○ ", t.status_style(InstanceStatus::Pending)),
            Span::styled("Pending  ", t.dim()),
            Span::styled("◔ ", t.status_style(InstanceStatus::Launching)),
            Span::styled("Launching  ", t.dim()),
            Span::styled("● ", t.status_style(InstanceStatus::Running)),
            Span::styled("Running", t.dim()),
        ]),
        Line::from(vec![
            Span::styled("  ✓ ", t.status_style(InstanceStatus::Complete)),
            Span::styled("Complete ", t.dim()),
            Span::styled("✗ ", t.status_style(InstanceStatus::Failed)),
            Span::styled("Failed", t.dim()),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled("  Press any key to close", t.dim())]),
    ];

    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(t.block_focused())
        .style(Style::default().bg(t.bg));

    let paragraph = Paragraph::new(help_text)
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

/// Render quit confirmation popup overlay
fn render_quit_confirm_popup(frame: &mut Frame) {
    let t = theme::theme();
    let area = centered_rect(30, 20, frame.area());

    // Clear the area behind the popup
    frame.render_widget(Clear, area);

    let key_style = t.key_badge();

    let text = vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            "Are you sure you want to quit?",
            t.text(),
        )]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "This will terminate instances and clean up resources.",
            t.dim(),
        )]),
        Line::from(""),
        Line::from(vec![
            Span::styled(" y ", key_style),
            Span::styled(" Yes  ", t.text()),
            Span::styled(" n ", key_style),
            Span::styled(" No ", t.text()),
        ]),
    ];

    let block = Block::default()
        .title(" Confirm Quit ")
        .borders(Borders::ALL)
        .border_style(t.block_focused())
        .style(Style::default().bg(t.bg));

    let paragraph = Paragraph::new(text)
        .block(block)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

/// Create a centered rectangle
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// Get status symbol for display
pub fn status_symbol(status: InstanceStatus) -> &'static str {
    match status {
        InstanceStatus::Pending => "○",
        InstanceStatus::Launching => "◔",
        InstanceStatus::Running => "●",
        InstanceStatus::Complete => "✓",
        InstanceStatus::Failed => "✗",
        InstanceStatus::Terminated => "⊘",
    }
}

/// Get status color
pub fn status_color(status: InstanceStatus) -> Color {
    theme::theme().status_color(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::InstanceStatus;
    use crate::tui::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn render_does_not_panic_with_empty_state() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new_loading(&[], 0, None);
        terminal.draw(|f| render(f, &mut app)).unwrap();
    }

    #[test]
    fn render_does_not_panic_with_instances() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let instances = vec!["m5.large".to_string(), "c5.xlarge".to_string()];
        let mut app = App::new_loading(&instances, 10, None);
        terminal.draw(|f| render(f, &mut app)).unwrap();
    }

    #[test]
    fn render_does_not_panic_with_tiny_terminal() {
        // Tiny terminal that could cause layout issues
        let backend = TestBackend::new(10, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let instances = vec!["m5.large".to_string()];
        let mut app = App::new_loading(&instances, 5, None);
        terminal.draw(|f| render(f, &mut app)).unwrap();
    }

    #[test]
    fn render_does_not_panic_with_very_long_instance_names() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let instances = vec![
            "this-is-a-very-long-instance-type-name-that-exceeds-normal-bounds".to_string(),
            "another-extremely-long-instance-name-for-testing-truncation".to_string(),
        ];
        let mut app = App::new_loading(&instances, 10, None);
        terminal.draw(|f| render(f, &mut app)).unwrap();
    }

    #[test]
    fn render_does_not_panic_with_unicode_content() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        // Instance names with unicode (unlikely in practice but test robustness)
        let instances = vec!["日本語インスタンス".to_string(), "🦀-instance".to_string()];
        let mut app = App::new_loading(&instances, 5, None);

        // Add some unicode to console output
        if let Some(state) = app.instances.data.get_mut("日本語インスタンス") {
            state.console_output.push_line("Building: こんにちは世界".to_string());
            state.console_output.push_line("Progress: 🚀🔥✓".to_string());
        }

        terminal.draw(|f| render(f, &mut app)).unwrap();
    }

    #[test]
    fn render_does_not_panic_with_all_status_types() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let instances = vec![
            "pending".to_string(),
            "launching".to_string(),
            "running".to_string(),
            "complete".to_string(),
            "failed".to_string(),
        ];
        let mut app = App::new_loading(&instances, 5, None);

        // Set different statuses
        if let Some(s) = app.instances.data.get_mut("pending") {
            s.status = InstanceStatus::Pending;
        }
        if let Some(s) = app.instances.data.get_mut("launching") {
            s.status = InstanceStatus::Launching;
        }
        if let Some(s) = app.instances.data.get_mut("running") {
            s.status = InstanceStatus::Running;
        }
        if let Some(s) = app.instances.data.get_mut("complete") {
            s.status = InstanceStatus::Complete;
        }
        if let Some(s) = app.instances.data.get_mut("failed") {
            s.status = InstanceStatus::Failed;
        }

        terminal.draw(|f| render(f, &mut app)).unwrap();
    }

    #[test]
    fn render_does_not_panic_with_help_popup() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new_loading(&["m5.large".to_string()], 5, None);
        app.ui.show_help = true;
        terminal.draw(|f| render(f, &mut app)).unwrap();
    }

    #[test]
    fn render_does_not_panic_with_quit_confirm_popup() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new_loading(&["m5.large".to_string()], 5, None);
        app.ui.show_quit_confirm = true;
        terminal.draw(|f| render(f, &mut app)).unwrap();
    }

    #[test]
    fn render_does_not_panic_with_large_log_buffer() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new_loading(&["m5.large".to_string()], 5, None);

        // Fill log buffer with many lines
        if let Some(state) = app.instances.data.get_mut("m5.large") {
            for i in 0..1000 {
                state
                    .console_output
                    .push_line(format!("Log line {}: some build output here", i));
            }
        }

        terminal.draw(|f| render(f, &mut app)).unwrap();
    }

    #[test]
    fn status_symbol_returns_correct_symbols() {
        assert_eq!(status_symbol(InstanceStatus::Pending), "○");
        assert_eq!(status_symbol(InstanceStatus::Launching), "◔");
        assert_eq!(status_symbol(InstanceStatus::Running), "●");
        assert_eq!(status_symbol(InstanceStatus::Complete), "✓");
        assert_eq!(status_symbol(InstanceStatus::Failed), "✗");
    }

    #[test]
    fn status_color_returns_theme_colors() {
        use crate::tui::theme;
        let t = theme::theme();
        assert_eq!(status_color(InstanceStatus::Pending), t.status_pending);
        assert_eq!(status_color(InstanceStatus::Launching), t.status_launching);
        assert_eq!(status_color(InstanceStatus::Running), t.status_running);
        assert_eq!(status_color(InstanceStatus::Complete), t.status_complete);
        assert_eq!(status_color(InstanceStatus::Failed), t.status_failed);
    }
}
