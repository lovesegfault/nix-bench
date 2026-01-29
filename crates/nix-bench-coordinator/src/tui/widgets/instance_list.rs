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
use throbber_widgets_tui::{BRAILLE_SIX, Throbber};

/// Calculate trend from duration history
/// Returns: -1 (faster), 0 (stable), 1 (slower)
fn calculate_trend(durations: &[f64]) -> i8 {
    if durations.len() < 2 {
        return 0; // Not enough data
    }
    // Compare first half average to second half average
    let mid = durations.len() / 2;
    let first_half_avg: f64 = durations[..mid].iter().sum::<f64>() / mid as f64;
    let second_half_avg: f64 =
        durations[mid..].iter().sum::<f64>() / (durations.len() - mid) as f64;

    let change_pct = (second_half_avg - first_half_avg) / first_half_avg;
    if change_pct < -0.05 {
        -1 // Getting faster (5%+ improvement)
    } else if change_pct > 0.05 {
        1 // Getting slower (5%+ regression)
    } else {
        0 // Stable (within 5%)
    }
}

/// Format duration with trend arrow
/// Returns (arrow, arrow_color, time_string)
fn format_trend_time(durations: &[f64]) -> (&'static str, Color, String) {
    let t = theme::theme();
    if durations.is_empty() {
        return (" ", t.fg_dim, "--.--s".to_string());
    }

    let avg = durations.iter().sum::<f64>() / durations.len() as f64;
    let trend = calculate_trend(durations);

    let (arrow, color) = match trend {
        -1 => ("↓", t.success), // Faster = green
        1 => ("↑", t.error),    // Slower = red
        _ => ("→", t.fg_dim),   // Stable = dim
    };

    (arrow, color, format!("{:>5.1}s", avg))
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

    // Trend+time display width: "↓ 999.9s" = 8 chars
    let time_width = 8;

    // Calculate available width (minus borders, highlight symbol, time display)
    let available_width = area.width.saturating_sub(5 + time_width as u16 + 2) as usize;

    let items: Vec<ListItem> = app
        .instances
        .order
        .iter()
        .filter_map(|instance_type| {
            let state = app.instances.data.get(instance_type)?;
            // Use space for running instances (throbber rendered separately)
            let symbol = if state.status == InstanceStatus::Running {
                " "
            } else {
                status_symbol(state.status)
            };
            let color = status_color(state.status);

            // Compact format: symbol instance_type runs trend+time
            let runs_str = format!("{}/{}", state.run_progress, state.total_runs);
            let max_name_len = available_width.saturating_sub(runs_str.len() + 4);

            let durations = state.durations();
            let (arrow, arrow_color, time_str) = format_trend_time(&durations);

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
                Span::styled(format!("{} ", arrow), Style::default().fg(arrow_color)),
                Span::styled(time_str, t.dim()),
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

    for (i, instance_type) in app.instances.order.iter().enumerate() {
        if i < scroll_offset || i >= scroll_offset + visible_count {
            continue; // Not visible
        }
        let state = match app.instances.data.get(instance_type) {
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

        if let Some(throbber_state) = app.scroll.throbber_states.get_mut(instance_type) {
            let throbber = Throbber::default()
                .throbber_set(BRAILLE_SIX)
                .style(Style::default().fg(t.status_running));
            frame.render_stateful_widget(throbber, throbber_area, throbber_state);
        }
    }

    // Return the scroll offset for mouse click calculation
    scroll_offset
}
