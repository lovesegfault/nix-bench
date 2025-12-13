//! Aggregate stats widget

use crate::orchestrator::InstanceStatus;
use crate::tui::app::App;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph},
};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let total_instances = app.instances.len();
    let completed = app
        .instances
        .values()
        .filter(|s| s.status == InstanceStatus::Complete)
        .count();
    let failed = app
        .instances
        .values()
        .filter(|s| s.status == InstanceStatus::Failed)
        .count();
    let running = app
        .instances
        .values()
        .filter(|s| s.status == InstanceStatus::Running)
        .count();

    let total_runs: u32 = app.instances.len() as u32 * app.total_runs;
    let completed_runs: u32 = app.instances.values().map(|s| s.run_progress).sum();

    // Find best and worst performers (by average duration)
    let mut instances_with_avg: Vec<(&String, f64)> = app
        .instances
        .iter()
        .filter(|(_, s)| !s.durations.is_empty())
        .map(|(k, s)| {
            let avg = s.durations.iter().sum::<f64>() / s.durations.len() as f64;
            (k, avg)
        })
        .collect();

    instances_with_avg.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    let best = instances_with_avg
        .first()
        .map(|(k, v)| format!("{}: {:.1}s", truncate(k, 15), v));
    let worst = instances_with_avg
        .last()
        .filter(|_| instances_with_avg.len() > 1)
        .map(|(k, v)| format!("{}: {:.1}s", truncate(k, 15), v));

    // Build status line with colors
    let status_line = Line::from(vec![
        Span::styled(" Instances: ", Style::default().bold()),
        Span::raw(format!("{} ", total_instances)),
        Span::styled("│", Style::default().fg(Color::DarkGray)),
        Span::styled(" Running: ", Style::default().fg(Color::Blue)),
        Span::raw(format!("{} ", running)),
        Span::styled("│", Style::default().fg(Color::DarkGray)),
        Span::styled(" Complete: ", Style::default().fg(Color::Green)),
        Span::raw(format!("{} ", completed)),
        Span::styled("│", Style::default().fg(Color::DarkGray)),
        Span::styled(" Failed: ", Style::default().fg(Color::Red)),
        Span::raw(format!("{} ", failed)),
        Span::styled("│", Style::default().fg(Color::DarkGray)),
        Span::styled(" Runs: ", Style::default().bold()),
        Span::raw(format!("{}/{}", completed_runs, total_runs)),
    ]);

    // Build performance line
    let mut perf_spans = vec![Span::raw(" ")];
    if let Some(b) = best {
        perf_spans.push(Span::styled("Best: ", Style::default().fg(Color::Green)));
        perf_spans.push(Span::raw(b));
    }
    if let Some(w) = worst {
        perf_spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
        perf_spans.push(Span::styled("Slowest: ", Style::default().fg(Color::Yellow)));
        perf_spans.push(Span::raw(w));
    }
    let perf_line = Line::from(perf_spans);

    let block = Block::default()
        .title(" Summary ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let text = vec![status_line, perf_line];
    let paragraph = Paragraph::new(text).block(block);

    frame.render_widget(paragraph, area);
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}…", &s[..max_len - 1])
    }
}
