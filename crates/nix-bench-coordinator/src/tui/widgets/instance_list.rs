//! Unified instance list widget with stats, throbbers, and performance bars

use crate::orchestrator::InstanceStatus;
use crate::tui::app::App;
use crate::tui::theme;
use crate::tui::truncate_str;
use crate::tui::ui::{status_color, status_symbol};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, ListState},
};
use throbber_widgets_tui::{Throbber, BRAILLE_SIX};

/// Generate a performance bar using Unicode block characters
/// ratio: 0.0 = fastest, 1.0 = slowest
fn performance_bar(ratio: f64, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let filled = (ratio * width as f64).round() as usize;
    let empty = width.saturating_sub(filled);
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
}

/// Get color for performance bar based on ratio (0.0 = fastest/green, 1.0 = slowest/red)
fn bar_color_for_ratio(ratio: f64) -> Color {
    let t = theme::theme();
    if ratio < 0.33 {
        t.success // Green - fast
    } else if ratio < 0.66 {
        t.warning // Yellow - medium
    } else {
        t.error // Red - slow
    }
}

/// Render the unified instance list and return the scroll offset for mouse click handling
pub fn render(frame: &mut Frame, area: Rect, app: &mut App) -> usize {
    let t = theme::theme();

    // Calculate R/C/F counts for title bar
    let running = app
        .instances
        .data
        .values()
        .filter(|s| s.status == InstanceStatus::Running)
        .count();
    let complete = app
        .instances
        .data
        .values()
        .filter(|s| s.status == InstanceStatus::Complete)
        .count();
    let failed = app
        .instances
        .data
        .values()
        .filter(|s| s.status == InstanceStatus::Failed)
        .count();

    // Calculate duration statistics for bar visualization
    let instances_with_avg: Vec<(&String, f64)> = app
        .instances
        .order
        .iter()
        .filter_map(|it| {
            let state = app.instances.data.get(it)?;
            let avg = if state.durations.is_empty() {
                f64::MAX
            } else {
                state.durations.iter().sum::<f64>() / state.durations.len() as f64
            };
            Some((it, avg))
        })
        .collect();

    let valid_avgs: Vec<f64> = instances_with_avg
        .iter()
        .filter(|(_, avg)| *avg != f64::MAX)
        .map(|(_, avg)| *avg)
        .collect();

    let min_avg = valid_avgs.iter().cloned().fold(f64::MAX, f64::min);
    let max_avg = valid_avgs.iter().cloned().fold(f64::MIN, f64::max);

    // Bar width
    let bar_width = 8;

    // Calculate available width (minus borders, highlight symbol, bar)
    let available_width = area.width.saturating_sub(5 + bar_width as u16 + 2) as usize;

    let items: Vec<ListItem> = instances_with_avg
        .iter()
        .filter_map(|(instance_type, avg)| {
            let state = app.instances.data.get(*instance_type)?;
            // Use space for running instances (throbber rendered separately)
            let symbol = if state.status == InstanceStatus::Running {
                " "
            } else {
                status_symbol(state.status)
            };
            let color = status_color(state.status);

            // Compact format: symbol instance_type runs bar
            let runs_str = format!("{}/{}", state.run_progress, state.total_runs);
            let max_name_len = available_width.saturating_sub(runs_str.len() + 4);

            // Calculate bar ratio
            let ratio = if *avg == f64::MAX || max_avg <= min_avg {
                0.0
            } else {
                ((avg - min_avg) / (max_avg - min_avg)).clamp(0.0, 1.0)
            };
            let bar = performance_bar(ratio, bar_width);
            let bar_color = bar_color_for_ratio(ratio);

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
                Span::styled(format!("{:>5} ", runs_str), t.dim()),
                Span::styled(bar, Style::default().fg(bar_color)),
            ]);

            Some(ListItem::new(line))
        })
        .collect();

    // Title with R/C/F counts
    let title = format!(
        " Instances ({}) │ R:{} C:{} F:{} ",
        app.instances.data.len(),
        running,
        complete,
        failed
    );

    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .title_style(t.title_unfocused())
                .borders(Borders::ALL)
                .border_style(t.block_unfocused()),
        )
        .highlight_style(t.selection().add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    list_state.select(Some(app.instances.selected_index));

    frame.render_stateful_widget(list, area, &mut list_state);
    let scroll_offset = list_state.offset();

    // Overlay throbbers for running instances
    let inner_x = area.x + 1; // Account for border
    let inner_y = area.y + 1; // Account for border
    let visible_count = (area.height.saturating_sub(2)) as usize; // Account for borders

    for (i, (instance_type, _)) in instances_with_avg.iter().enumerate() {
        if i < scroll_offset || i >= scroll_offset + visible_count {
            continue; // Not visible
        }
        let state = match app.instances.data.get(*instance_type) {
            Some(s) => s,
            None => continue,
        };
        if state.status != InstanceStatus::Running {
            continue; // Not running
        }

        let row_y = inner_y + (i - scroll_offset) as u16;

        // Offset by 2 for the highlight symbol "▶ "
        let throbber_x = inner_x + 2;
        let throbber_area = Rect::new(throbber_x, row_y, 1, 1);

        if let Some(throbber_state) = app.scroll.throbber_states.get_mut(*instance_type) {
            let throbber = Throbber::default()
                .throbber_set(BRAILLE_SIX)
                .style(Style::default().fg(t.status_running));
            frame.render_stateful_widget(throbber, throbber_area, throbber_state);
        }
    }

    // Return the scroll offset for mouse click calculation
    scroll_offset
}
