//! Instance detail widget

use crate::orchestrator::InstanceState;
use crate::tui::app::PanelFocus;
use crate::tui::theme;
use crate::tui::ui::{status_color, status_symbol};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, Table},
};
use tui_scrollview::ScrollViewState;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    instance: &InstanceState,
    total_runs: u32,
    scroll_state: &mut ScrollViewState,
    auto_follow: bool,
    focus: PanelFocus,
) -> Rect {
    let t = theme::theme();

    let block = Block::default()
        .title(format!(" {} ", instance.instance_type))
        .title_style(t.title_unfocused())
        .borders(Borders::ALL)
        .border_style(t.block_unfocused());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Split inner area - info, run history, and logs
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // Info section
            Constraint::Length(8), // Run history (compact)
            Constraint::Min(5),    // Build logs
        ])
        .split(inner);

    // Instance info
    render_info(frame, chunks[0], instance);

    // Run history table
    render_run_history(frame, chunks[1], instance, total_runs);

    // Build logs (scrollable)
    render_logs(frame, chunks[2], instance, scroll_state, auto_follow, focus);

    // Return the logs area for mouse detection
    chunks[2]
}

fn render_info(frame: &mut Frame, area: Rect, instance: &InstanceState) {
    let t = theme::theme();
    let status_sym = status_symbol(instance.status);
    let status_col = status_color(instance.status);

    // Calculate stats
    let avg_duration = if !instance.durations.is_empty() {
        instance.durations.iter().sum::<f64>() / instance.durations.len() as f64
    } else {
        0.0
    };

    let min_duration = instance
        .durations
        .iter()
        .cloned()
        .filter(|x| x.is_finite())
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(0.0);

    let max_duration = instance
        .durations
        .iter()
        .cloned()
        .filter(|x| x.is_finite())
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(0.0);

    let info_text = vec![
        Line::from(vec![
            Span::styled("  Status: ", t.bold()),
            Span::styled(
                format!("{} {:?}", status_sym, instance.status),
                Style::default().fg(status_col),
            ),
            Span::styled("   ", t.text()),
            Span::styled("System: ", t.bold()),
            Span::styled(&instance.system, t.text()),
        ]),
        Line::from(vec![
            Span::styled("  Instance ID: ", t.bold()),
            Span::styled(&instance.instance_id, Style::default().fg(t.accent_primary)),
        ]),
        Line::from(vec![
            Span::styled("  Public IP: ", t.bold()),
            Span::styled(instance.public_ip.as_deref().unwrap_or("-"), t.text()),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Stats: ", t.bold()),
            Span::styled(
                format!(
                    "Avg: {:.1}s  Min: {:.1}s  Max: {:.1}s",
                    avg_duration, min_duration, max_duration
                ),
                t.text(),
            ),
        ]),
    ];

    let info = Paragraph::new(info_text);
    frame.render_widget(info, area);
}

fn render_run_history(frame: &mut Frame, area: Rect, instance: &InstanceState, total_runs: u32) {
    let t = theme::theme();

    // Calculate available height for rows (minus header and border)
    let visible_rows = area.height.saturating_sub(2) as usize;

    // Calculate average for comparison
    let avg_duration = if !instance.durations.is_empty() {
        instance.durations.iter().sum::<f64>() / instance.durations.len() as f64
    } else {
        0.0
    };

    // Calculate scroll offset to keep current/latest run visible
    let current_run = instance.run_progress as usize + 1;
    let scroll_offset = if total_runs as usize <= visible_rows || current_run <= visible_rows {
        0 // All rows fit, or current run is in first visible chunk
    } else {
        // Scroll to keep current run visible (near bottom of visible area)
        (current_run.saturating_sub(visible_rows.saturating_sub(1))).min(
            (total_runs as usize).saturating_sub(visible_rows),
        )
    };

    let needs_scrollbar = total_runs as usize > visible_rows;

    // Build all rows
    let all_rows: Vec<Row> = (1..=total_runs)
        .map(|run| {
            let (duration_str, status_str, status_style, diff_str) =
                if let Some(&duration) = instance.durations.get(run as usize - 1) {
                    let diff = duration - avg_duration;
                    let diff_str = if instance.durations.len() > 1 {
                        if diff > 0.0 {
                            format!("+{:.1}s", diff)
                        } else {
                            format!("{:.1}s", diff)
                        }
                    } else {
                        "-".to_string()
                    };

                    (
                        format!("{:.1}s", duration),
                        "✓",
                        t.success_style(),
                        diff_str,
                    )
                } else if run == instance.run_progress && run <= total_runs {
                    (
                        "running...".to_string(),
                        "◐",
                        t.info_style(),
                        "-".to_string(),
                    )
                } else if run <= instance.run_progress {
                    (
                        "-".to_string(),
                        "✓",
                        t.success_style(),
                        "-".to_string(),
                    )
                } else {
                    (
                        "-".to_string(),
                        "○",
                        t.dim(),
                        "-".to_string(),
                    )
                };

            let diff_style = if diff_str.starts_with('+') {
                t.error_style()
            } else if diff_str.starts_with('-') && diff_str != "-" {
                t.success_style()
            } else {
                t.dim()
            };

            Row::new(vec![
                Cell::from(run.to_string()).style(t.text()),
                Cell::from(duration_str).style(t.text()),
                Cell::from(status_str).style(status_style),
                Cell::from(diff_str).style(diff_style),
            ])
        })
        .collect();

    // Slice to visible rows based on scroll offset
    let visible_row_range = scroll_offset..all_rows.len().min(scroll_offset + visible_rows);
    let visible_rows_data: Vec<Row> = all_rows
        .into_iter()
        .enumerate()
        .filter(|(i, _)| visible_row_range.contains(i))
        .map(|(_, row)| row)
        .collect();

    let header_cells = ["#", "Duration", "Status", "Diff"]
        .iter()
        .map(|h| Cell::from(*h).style(t.table_header()));
    let header = Row::new(header_cells).height(1);

    // Show scroll position in title if scrolling
    let title = if needs_scrollbar {
        format!(
            " Run History ({}-{}/{}) ",
            scroll_offset + 1,
            (scroll_offset + visible_rows).min(total_runs as usize),
            total_runs
        )
    } else {
        " Run History ".to_string()
    };

    // Adjust table width to leave room for scrollbar
    let table_width = if needs_scrollbar {
        area.width.saturating_sub(1)
    } else {
        area.width
    };

    let table = Table::new(
        visible_rows_data,
        [
            Constraint::Length(4),
            Constraint::Length(12),
            Constraint::Length(8),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title(title)
            .title_style(t.title_unfocused())
            .borders(Borders::TOP)
            .border_style(t.block_unfocused()),
    );

    let table_area = Rect::new(area.x, area.y, table_width, area.height);
    frame.render_widget(table, table_area);

    // Render scrollbar if needed
    if needs_scrollbar {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_symbol("█")
            .track_symbol(Some("░"))
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_style(t.scrollbar_thumb_style())
            .track_style(t.scrollbar_track_style());

        let scrollbar_area = Rect::new(
            area.x + area.width.saturating_sub(1),
            area.y + 1, // Skip header
            1,
            area.height.saturating_sub(1),
        );

        let max_scroll = (total_runs as usize).saturating_sub(visible_rows);
        let mut scrollbar_state =
            ratatui::widgets::ScrollbarState::new(max_scroll).position(scroll_offset);

        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }
}

fn render_logs(
    frame: &mut Frame,
    area: Rect,
    instance: &InstanceState,
    scroll_state: &mut ScrollViewState,
    auto_follow: bool,
    focus: PanelFocus,
) {
    let t = theme::theme();
    let focused = focus == PanelFocus::BuildOutput;

    let (border_style, title_style) = if focused {
        (t.block_focused(), t.block_focused())
    } else {
        (t.block_unfocused(), t.title_unfocused())
    };

    let title = if focused {
        " Build Output (focused) "
    } else {
        " Build Output "
    };

    let block = Block::default()
        .title(title)
        .title_style(title_style)
        .borders(Borders::TOP)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Get content - LogBuffer stores lines directly, no need to split
    let has_content = !instance.console_output.is_empty();
    let placeholder = if instance.instance_id.is_empty() {
        "Waiting for instance to launch..."
    } else {
        "Waiting for build logs..."
    };

    let lines: Vec<&str> = if has_content {
        instance.console_output.lines().collect()
    } else {
        vec![placeholder]
    };
    let line_count = lines.len();
    let content_height = line_count as u16;
    let needs_scrollbar = content_height > inner.height;

    // Calculate content area (leave room for scrollbar if needed)
    let content_width = if needs_scrollbar {
        inner.width.saturating_sub(1)
    } else {
        inner.width
    };

    // Virtualized rendering: only render visible lines for O(viewport) instead of O(total_lines)
    let viewport_height = inner.height as usize;

    // If auto-follow is enabled and there's content, scroll to bottom
    if auto_follow && line_count > viewport_height {
        // Set scroll offset to show the last viewport_height lines
        let max_scroll = line_count.saturating_sub(viewport_height);
        scroll_state.set_offset(ratatui::layout::Position::new(0, max_scroll as u16));
    }

    // Get current scroll position and clamp to valid range
    let max_scroll = line_count.saturating_sub(viewport_height);
    let raw_scroll_y = scroll_state.offset().y as usize;
    let scroll_y = raw_scroll_y.min(max_scroll);

    // Update scroll state if it was out of bounds (prevents scrolling past end)
    if raw_scroll_y != scroll_y {
        scroll_state.set_offset(ratatui::layout::Position::new(0, scroll_y as u16));
    }

    // Calculate visible line range (virtualized - only render what's on screen)
    let visible_start = scroll_y;
    let visible_end = (scroll_y + viewport_height).min(line_count);

    // Render only visible lines directly to frame (bypasses ScrollView overhead)
    for (viewport_row, line_idx) in (visible_start..visible_end).enumerate() {
        let line_area = Rect::new(inner.x, inner.y + viewport_row as u16, content_width, 1);
        frame.render_widget(Paragraph::new(lines[line_idx]).style(t.dim()), line_area);
    }

    // Render themed scrollbar if content overflows
    if needs_scrollbar {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_symbol("█")
            .track_symbol(Some("░"))
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_style(t.scrollbar_thumb_style())
            .track_style(t.scrollbar_track_style());

        let scrollbar_area = Rect::new(
            inner.x + inner.width.saturating_sub(1),
            inner.y,
            1,
            inner.height,
        );

        // Create a scrollbar state from scroll position
        let scroll_pos = scroll_state.offset().y as usize;
        let max_scroll = (content_height.saturating_sub(inner.height)) as usize;
        let mut scrollbar_state =
            ratatui::widgets::ScrollbarState::new(max_scroll).position(scroll_pos);

        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }
}
