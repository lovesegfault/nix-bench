//! Instance list widget

use crate::tui::app::App;
use crate::tui::ui::{status_color, status_symbol};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, ListState},
};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    // Calculate available width (minus borders and highlight symbol)
    let available_width = area.width.saturating_sub(5) as usize;

    let items: Vec<ListItem> = app
        .instance_order
        .iter()
        .map(|instance_type| {
            let state = app.instances.get(instance_type).unwrap();
            let symbol = status_symbol(state.status);
            let color = status_color(state.status);

            // Compact format: symbol instance_type runs
            // e.g., "● c8g.48xlarge 3/10"
            let runs_str = format!("{}/{}", state.run_progress, state.total_runs);
            let max_name_len = available_width.saturating_sub(runs_str.len() + 4); // symbol + spaces

            let line = Line::from(vec![
                Span::styled(format!("{} ", symbol), Style::default().fg(color)),
                Span::raw(format!(
                    "{:<width$} ",
                    truncate_instance_type(instance_type, max_name_len),
                    width = max_name_len
                )),
                Span::styled(runs_str, Style::default().fg(Color::DarkGray)),
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
fn truncate_instance_type(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len > 1 {
        format!("{}…", &s[..max_len - 1])
    } else {
        "…".to_string()
    }
}
