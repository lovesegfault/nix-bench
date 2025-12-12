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

    let total_runs: u32 = app.instances.values().map(|s| s.total_runs).sum();
    let completed_runs: u32 = app.instances.values().map(|s| s.run_progress).sum();

    // Find best and worst performers
    let mut instances_with_avg: Vec<(&String, f64)> = app
        .instances
        .iter()
        .filter(|(_, s)| !s.durations.is_empty())
        .map(|(k, s)| {
            let avg = s.durations.iter().sum::<f64>() / s.durations.len() as f64;
            (k, avg)
        })
        .collect();

    instances_with_avg.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

    let best = instances_with_avg.first().map(|(k, v)| format!("{} ({:.1}s)", k, v));
    let worst = instances_with_avg.last().map(|(k, v)| format!("{} ({:.1}s)", k, v));

    let stats_text = format!(
        "Instances: {} | Runs: {}/{} | Complete: {} | Failed: {}{}{}",
        total_instances,
        completed_runs,
        total_runs,
        completed,
        failed,
        best.map(|b| format!(" | Best: {}", b)).unwrap_or_default(),
        worst
            .filter(|_| instances_with_avg.len() > 1)
            .map(|w| format!(" | Slowest: {}", w))
            .unwrap_or_default(),
    );

    let block = Block::default()
        .title(" Aggregate Stats ")
        .borders(Borders::ALL);

    let paragraph = Paragraph::new(stats_text)
        .block(block)
        .alignment(Alignment::Center);

    frame.render_widget(paragraph, area);
}
