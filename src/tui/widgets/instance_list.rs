//! Instance list widget

use crate::tui::app::App;
use crate::tui::theme;
use crate::tui::truncate_str;
use crate::tui::ui::{status_color, status_symbol};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, ListState},
};

/// Render the instance list and return the scroll offset for mouse click handling
pub fn render(frame: &mut Frame, area: Rect, app: &App) -> usize {
    let t = theme::theme();

    // Calculate available width (minus borders and highlight symbol)
    let available_width = area.width.saturating_sub(5) as usize;

    let items: Vec<ListItem> = app
        .instances
        .order
        .iter()
        .filter_map(|instance_type| {
            let state = app.instances.data.get(instance_type)?;
            let symbol = status_symbol(state.status);
            let color = status_color(state.status);

            // Compact format: symbol instance_type runs
            // e.g., "● c8g.48xlarge 3/10"
            let runs_str = format!("{}/{}", state.run_progress, state.total_runs);
            let max_name_len = available_width.saturating_sub(runs_str.len() + 4); // symbol + spaces

            let line = Line::from(vec![
                Span::styled(format!("{} ", symbol), Style::default().fg(color)),
                Span::styled(
                    format!(
                        "{:<width$} ",
                        truncate_str(instance_type, max_name_len),
                        width = max_name_len
                    ),
                    t.text(),
                ),
                Span::styled(runs_str, t.dim()),
            ]);

            Some(ListItem::new(line))
        })
        .collect();

    let title = format!(" Instances ({}) ", app.instances.data.len());
    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(t.block_unfocused()),
        )
        .highlight_style(t.selection().add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    list_state.select(Some(app.instances.selected_index));

    frame.render_stateful_widget(list, area, &mut list_state);

    // Return the scroll offset for mouse click calculation
    list_state.offset()
}
