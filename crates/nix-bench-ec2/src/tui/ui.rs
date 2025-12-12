//! UI rendering

use crate::orchestrator::InstanceStatus;
use crate::tui::app::App;
use crate::tui::widgets::{aggregate_stats, instance_detail, instance_list};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph},
};

/// Render the entire UI
pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),     // Main content
            Constraint::Length(3),   // Aggregate stats
        ])
        .split(frame.area());

    // Main content: instances list and detail
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(40),  // Instance list
            Constraint::Percentage(60),  // Instance detail
        ])
        .split(chunks[0]);

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
    aggregate_stats::render(frame, chunks[1], app);
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
