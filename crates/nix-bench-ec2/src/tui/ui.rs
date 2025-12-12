//! UI rendering

use crate::orchestrator::InstanceStatus;
use crate::tui::app::App;
use crate::tui::widgets::{aggregate_stats, instance_detail, instance_list};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use tui_logger::TuiLoggerWidget;

/// Render the entire UI
pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),  // Header/title bar
            Constraint::Min(10),    // Main content
            Constraint::Length(3),  // Aggregate stats
            Constraint::Length(8),  // Log panel
            Constraint::Length(1),  // Help bar
        ])
        .split(frame.area());

    // Render header with elapsed time
    render_header(frame, chunks[0], app);

    // Main content: instances list and detail
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(30),        // Instance list (fixed width)
            Constraint::Percentage(100), // Instance detail (fill remaining)
        ])
        .split(chunks[1]);

    // Render instance list
    instance_list::render(frame, main_chunks[0], app);

    // Render instance detail
    if let Some(instance) = app.selected_instance() {
        instance_detail::render(frame, main_chunks[1], instance, app.total_runs);
    } else {
        let block = Block::default()
            .title(" Details ")
            .borders(Borders::ALL);
        let paragraph = Paragraph::new("No instance selected")
            .block(block)
            .alignment(Alignment::Center);
        frame.render_widget(paragraph, main_chunks[1]);
    }

    // Render aggregate stats
    aggregate_stats::render(frame, chunks[2], app);

    // Render log panel
    render_log_panel(frame, chunks[3]);

    // Render help bar
    render_help_bar(frame, chunks[4]);

    // Render help popup if toggled
    if app.show_help {
        render_help_popup(frame);
    }

    // Render logs popup if toggled
    if app.show_logs {
        render_logs_popup(frame, app);
    }
}

/// Render the header bar with title and elapsed time
fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    use crate::tui::app::InitPhase;

    let elapsed = app.elapsed_str();

    let header_text = if app.is_initializing() {
        format!(
            " nix-bench-ec2 │ {} │ Elapsed: {} ",
            app.init_phase.message(),
            elapsed
        )
    } else {
        let completion = app.completion_percentage();
        let remaining = app.estimated_remaining_str();
        format!(
            " nix-bench-ec2 │ Elapsed: {} │ Progress: {:.1}% │ ETA: {} ",
            elapsed, completion, remaining
        )
    };

    let style = match &app.init_phase {
        InitPhase::Completed => Style::default().fg(Color::Black).bg(Color::Green),
        InitPhase::Failed(_) => Style::default().fg(Color::White).bg(Color::Red),
        InitPhase::Running if app.all_complete() => {
            Style::default().fg(Color::Black).bg(Color::Green)
        }
        _ => Style::default().fg(Color::White).bg(Color::Blue),
    };

    let header = Paragraph::new(header_text).style(style);
    frame.render_widget(header, area);
}

/// Render the log panel
fn render_log_panel(frame: &mut Frame, area: Rect) {
    let log_widget = TuiLoggerWidget::default()
        .block(
            Block::default()
                .title(" Logs ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .style(Style::default().fg(Color::White));

    frame.render_widget(log_widget, area);
}

/// Render the help bar at the bottom
fn render_help_bar(frame: &mut Frame, area: Rect) {
    let help_text = Line::from(vec![
        Span::styled(" ↑/k ", Style::default().fg(Color::Black).bg(Color::Gray)),
        Span::raw(" Up "),
        Span::styled(" ↓/j ", Style::default().fg(Color::Black).bg(Color::Gray)),
        Span::raw(" Down "),
        Span::styled(" l ", Style::default().fg(Color::Black).bg(Color::Gray)),
        Span::raw(" Logs "),
        Span::styled(" ? ", Style::default().fg(Color::Black).bg(Color::Gray)),
        Span::raw(" Help "),
        Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::Gray)),
        Span::raw(" Quit "),
    ]);

    let help_bar = Paragraph::new(help_text);
    frame.render_widget(help_bar, area);
}

/// Render help popup overlay
fn render_help_popup(frame: &mut Frame) {
    let area = centered_rect(50, 60, frame.area());

    // Clear the area behind the popup
    frame.render_widget(Clear, area);

    let help_text = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Keyboard Shortcuts", Style::default().bold()),
        ]),
        Line::from(""),
        Line::from("  ↑ / k        Move selection up"),
        Line::from("  ↓ / j        Move selection down"),
        Line::from("  Home         Jump to first instance"),
        Line::from("  End          Jump to last instance"),
        Line::from(""),
        Line::from("  ? / F1       Toggle this help"),
        Line::from("  q / Esc      Quit (cleanup resources)"),
        Line::from("  Ctrl+C       Force quit (cleanup resources)"),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Status Icons", Style::default().bold()),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  ○ ", Style::default().fg(Color::Gray)),
            Span::raw("Pending    "),
            Span::styled("◔ ", Style::default().fg(Color::Yellow)),
            Span::raw("Launching"),
        ]),
        Line::from(vec![
            Span::styled("  ● ", Style::default().fg(Color::Blue)),
            Span::raw("Running    "),
            Span::styled("✓ ", Style::default().fg(Color::Green)),
            Span::raw("Complete"),
        ]),
        Line::from(vec![
            Span::styled("  ✗ ", Style::default().fg(Color::Red)),
            Span::raw("Failed"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Press any key to close", Style::default().fg(Color::DarkGray)),
        ]),
    ];

    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black));

    let paragraph = Paragraph::new(help_text).block(block).wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

/// Render logs popup overlay
fn render_logs_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(70, 80, frame.area());

    // Clear the area behind the popup
    frame.render_widget(Clear, area);

    let instance = app.selected_instance();
    let instance_type = app
        .instance_order
        .get(app.selected_index)
        .map(|s| s.as_str())
        .unwrap_or("none");

    let title = format!(" Logs: {} ", instance_type);

    let log_lines = if let Some(inst) = instance {
        if inst.instance_id.is_empty() {
            vec![
                Line::from(""),
                Line::from("  Instance not yet launched."),
                Line::from(""),
                Line::from("  Waiting for initialization to complete..."),
            ]
        } else {
            vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Instance ID: ", Style::default().bold()),
                    Span::styled(&inst.instance_id, Style::default().fg(Color::Cyan)),
                ]),
                Line::from(vec![
                    Span::styled("  Public IP:   ", Style::default().bold()),
                    Span::raw(inst.public_ip.as_deref().unwrap_or("pending...")),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  View bootstrap logs:", Style::default().bold()),
                ]),
                Line::from(""),
                Line::from(format!(
                    "    aws ssm start-session --target {}",
                    inst.instance_id
                )),
                Line::from("    sudo tail -f /var/log/nix-bench-bootstrap.log"),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Or via SSH (if public IP available):", Style::default().bold()),
                ]),
                Line::from(""),
                Line::from(format!(
                    "    ssh ec2-user@{}",
                    inst.public_ip.as_deref().unwrap_or("<pending>")
                )),
                Line::from("    sudo tail -f /var/log/nix-bench-bootstrap.log"),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  View nix build output:", Style::default().bold()),
                ]),
                Line::from(""),
                Line::from("    journalctl -u nix-bench-agent -f"),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Press 'l' to close", Style::default().fg(Color::DarkGray)),
                ]),
            ]
        }
    } else {
        vec![
            Line::from(""),
            Line::from("  No instance selected."),
        ]
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .style(Style::default().bg(Color::Black));

    let paragraph = Paragraph::new(log_lines).block(block).wrap(Wrap { trim: false });

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
    }
}

/// Get status color
pub fn status_color(status: InstanceStatus) -> Color {
    match status {
        InstanceStatus::Pending => Color::Gray,
        InstanceStatus::Launching => Color::Yellow,
        InstanceStatus::Running => Color::Blue,
        InstanceStatus::Complete => Color::Green,
        InstanceStatus::Failed => Color::Red,
    }
}
