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

            // Progress bar
            let progress = if state.total_runs > 0 {
                let filled = (state.run_progress as f64 / state.total_runs as f64 * 10.0) as usize;
                let empty = 10 - filled;
                format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
            } else {
                "[░░░░░░░░░░]".to_string()
            };

            let text = format!(
                "{} {:<20} {}",
                symbol, instance_type, progress
            );

            let style = Style::default().fg(color);
            ListItem::new(text).style(style)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Instances ")
                .borders(Borders::ALL),
        )
        .highlight_style(Style::default().bg(Color::DarkGray).bold())
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    list_state.select(Some(app.selected_index));

    frame.render_stateful_widget(list, area, &mut list_state);

    // Render legend at bottom of list area
    let legend_area = Rect {
        x: area.x + 1,
        y: area.y + area.height - 2,
        width: area.width - 2,
        height: 1,
    };

    if legend_area.y > area.y + 2 {
        let legend = Line::from(vec![
            Span::styled("● ", Style::default().fg(Color::Blue)),
            Span::raw("Running  "),
            Span::styled("○ ", Style::default().fg(Color::Gray)),
            Span::raw("Pending  "),
            Span::styled("✓ ", Style::default().fg(Color::Green)),
            Span::raw("Done  "),
            Span::styled("✗ ", Style::default().fg(Color::Red)),
            Span::raw("Failed"),
        ]);
        frame.render_widget(legend, legend_area);
    }
}
