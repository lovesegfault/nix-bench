//! Instance list widget

use crate::tui::app::App;
use crate::tui::ui::{status_color, status_symbol};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, ListState},
};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .instance_order
        .iter()
        .map(|instance_type| {
            let state = app.instances.get(instance_type).unwrap();
            let symbol = status_symbol(state.status);
            let color = status_color(state.status);

            // Progress bar (10 chars)
            let progress_pct = if state.total_runs > 0 {
                state.run_progress as f64 / state.total_runs as f64
            } else {
                0.0
            };
            let filled = (progress_pct * 10.0) as usize;
            let empty = 10 - filled;
            let progress_bar = format!("[{}{}]", "█".repeat(filled), "░".repeat(empty));

            // Arch indicator
            let arch = if state.system.contains("aarch64") {
                "ARM"
            } else {
                "x86"
            };

            // Format: symbol instance_type [progress] arch runs
            let line = Line::from(vec![
                Span::styled(format!("{} ", symbol), Style::default().fg(color)),
                Span::raw(format!("{:<18} ", truncate_instance_type(instance_type))),
                Span::styled(progress_bar, Style::default().fg(color)),
                Span::raw(format!(" {} ", arch)),
                Span::styled(
                    format!("{}/{}", state.run_progress, state.total_runs),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);

            ListItem::new(line)
        })
        .collect();

    let title = format!(" Instances ({}) ", app.instances.len());
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    list_state.select(Some(app.selected_index));

    frame.render_stateful_widget(list, area, &mut list_state);
}

/// Truncate instance type to fit in display
fn truncate_instance_type(s: &str) -> String {
    if s.len() <= 18 {
        s.to_string()
    } else {
        format!("{}…", &s[..17])
    }
}
