//! Instance detail widget

use crate::orchestrator::InstanceState;
use crate::tui::ui::{status_color, status_symbol};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};

pub fn render(frame: &mut Frame, area: Rect, instance: &InstanceState, total_runs: u32) {
    let block = Block::default()
        .title(format!(" Details: {} ", instance.instance_type))
        .borders(Borders::ALL);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Split inner area
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),  // Info
            Constraint::Min(5),     // Run history
        ])
        .split(inner);

    // Instance info
    let info_text = vec![
        Line::from(vec![
            Span::styled("Type: ", Style::default().bold()),
            Span::raw(&instance.instance_type),
        ]),
        Line::from(vec![
            Span::styled("System: ", Style::default().bold()),
            Span::raw(&instance.system),
        ]),
        Line::from(vec![
            Span::styled("Status: ", Style::default().bold()),
            Span::styled(
                format!("{} {:?}", status_symbol(instance.status), instance.status),
                Style::default().fg(status_color(instance.status)),
            ),
        ]),
    ];

    let info = Paragraph::new(info_text);
    frame.render_widget(info, chunks[0]);

    // Run history table
    let header_cells = ["#", "Duration", "Status"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().bold()));
    let header = Row::new(header_cells).height(1);

    let rows: Vec<Row> = (1..=total_runs)
        .map(|run| {
            let duration = instance
                .durations
                .get(run as usize - 1)
                .map(|d| format!("{:.1}s", d))
                .unwrap_or_else(|| {
                    if run == instance.run_progress + 1 && instance.run_progress < total_runs {
                        "running...".to_string()
                    } else if run <= instance.run_progress {
                        "done".to_string()
                    } else {
                        "-".to_string()
                    }
                });

            let status = if run <= instance.run_progress {
                if instance.durations.get(run as usize - 1).is_some() {
                    "✓"
                } else {
                    "✓"
                }
            } else if run == instance.run_progress + 1 {
                "◐"
            } else {
                "-"
            };

            Row::new(vec![
                Cell::from(run.to_string()),
                Cell::from(duration),
                Cell::from(status),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(12),
            Constraint::Length(8),
        ],
    )
    .header(header)
    .block(Block::default().title("Run History").borders(Borders::TOP));

    frame.render_widget(table, chunks[1]);
}
