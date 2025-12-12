//! Instance detail widget

use crate::orchestrator::InstanceState;
use crate::tui::ui::{status_color, status_symbol};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};

pub fn render(frame: &mut Frame, area: Rect, instance: &InstanceState, total_runs: u32) {
    let block = Block::default()
        .title(format!(" {} ", instance.instance_type))
        .borders(Borders::ALL);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Split inner area
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // Info section
            Constraint::Length(1), // Separator
            Constraint::Min(5),    // Run history
        ])
        .split(inner);

    // Instance info
    render_info(frame, chunks[0], instance);

    // Run history table
    render_run_history(frame, chunks[2], instance, total_runs);
}

fn render_info(frame: &mut Frame, area: Rect, instance: &InstanceState) {
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
        .min_by(|a, b| a.partial_cmp(b).unwrap())
        .unwrap_or(0.0);

    let max_duration = instance
        .durations
        .iter()
        .cloned()
        .max_by(|a, b| a.partial_cmp(b).unwrap())
        .unwrap_or(0.0);

    let info_text = vec![
        Line::from(vec![
            Span::styled("  Status: ", Style::default().bold()),
            Span::styled(
                format!("{} {:?}", status_sym, instance.status),
                Style::default().fg(status_col),
            ),
            Span::raw("   "),
            Span::styled("System: ", Style::default().bold()),
            Span::raw(&instance.system),
        ]),
        Line::from(vec![
            Span::styled("  Instance ID: ", Style::default().bold()),
            Span::styled(&instance.instance_id, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("  Public IP: ", Style::default().bold()),
            Span::raw(instance.public_ip.as_deref().unwrap_or("-")),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Stats: ", Style::default().bold()),
            Span::raw(format!(
                "Avg: {:.1}s  Min: {:.1}s  Max: {:.1}s",
                avg_duration, min_duration, max_duration
            )),
        ]),
    ];

    let info = Paragraph::new(info_text);
    frame.render_widget(info, area);
}

fn render_run_history(frame: &mut Frame, area: Rect, instance: &InstanceState, total_runs: u32) {
    let header_cells = ["#", "Duration", "Status", "Diff"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().bold().fg(Color::Yellow)));
    let header = Row::new(header_cells).height(1);

    // Calculate average for comparison
    let avg_duration = if !instance.durations.is_empty() {
        instance.durations.iter().sum::<f64>() / instance.durations.len() as f64
    } else {
        0.0
    };

    let rows: Vec<Row> = (1..=total_runs)
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
                        Style::default().fg(Color::Green),
                        diff_str,
                    )
                } else if run == instance.run_progress + 1 && run <= total_runs {
                    (
                        "running...".to_string(),
                        "◐",
                        Style::default().fg(Color::Blue),
                        "-".to_string(),
                    )
                } else if run <= instance.run_progress {
                    (
                        "-".to_string(),
                        "✓",
                        Style::default().fg(Color::Green),
                        "-".to_string(),
                    )
                } else {
                    (
                        "-".to_string(),
                        "○",
                        Style::default().fg(Color::DarkGray),
                        "-".to_string(),
                    )
                };

            let diff_style = if diff_str.starts_with('+') {
                Style::default().fg(Color::Red)
            } else if diff_str.starts_with('-') && diff_str != "-" {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            Row::new(vec![
                Cell::from(run.to_string()),
                Cell::from(duration_str),
                Cell::from(status_str).style(status_style),
                Cell::from(diff_str).style(diff_style),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
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
            .title(" Run History ")
            .borders(Borders::TOP),
    );

    frame.render_widget(table, area);
}
