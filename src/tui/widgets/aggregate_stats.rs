//! Aggregate stats widget (vertical layout with throbbers)

use crate::orchestrator::InstanceStatus;
use crate::tui::app::App;
use crate::tui::theme;
use crate::tui::truncate_str;
use crate::tui::ui::{status_color, status_symbol};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph},
};
use throbber_widgets_tui::{Throbber, BRAILLE_SIX};

/// Render the vertical stats panel
pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let t = theme::theme();

    let block = Block::default()
        .title(" Stats ")
        .borders(Borders::ALL)
        .border_style(t.block_unfocused());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Collect and sort instances by average duration (fastest first)
    let mut instances_with_stats: Vec<(&String, &crate::orchestrator::InstanceState, f64)> = app
        .instances
        .data
        .iter()
        .map(|(k, s)| {
            let avg = if s.durations.is_empty() {
                f64::MAX // Put instances without data at the end
            } else {
                s.durations.iter().sum::<f64>() / s.durations.len() as f64
            };
            (k, s, avg)
        })
        .collect();

    instances_with_stats.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));

    // Calculate how many instances we can display (2 lines per instance + 2 summary lines at top)
    let available_height = inner.height as usize;
    let summary_lines = 2;
    let lines_per_instance = 2;
    let max_instances = available_height.saturating_sub(summary_lines) / lines_per_instance;

    // Render summary at the top
    let total_runs: u32 = (app.instances.data.len() as u32).saturating_mul(app.context.total_runs);
    let completed_runs: u32 = app.instances.data.values().map(|s| s.run_progress).sum();
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

    let summary_line1 = Line::from(vec![
        Span::styled(" Runs: ", t.bold()),
        Span::styled(format!("{}/{}", completed_runs, total_runs), t.text()),
    ]);

    let summary_line2 = Line::from(vec![
        Span::styled(" R:", Style::default().fg(t.status_running)),
        Span::styled(format!("{} ", running), t.text()),
        Span::styled("C:", Style::default().fg(t.status_complete)),
        Span::styled(format!("{} ", complete), t.text()),
        Span::styled("F:", Style::default().fg(t.status_failed)),
        Span::styled(format!("{}", failed), t.text()),
    ]);

    let summary_area = Rect::new(inner.x, inner.y, inner.width, 2);
    frame.render_widget(
        Paragraph::new(vec![summary_line1, summary_line2]),
        summary_area,
    );

    // Render each instance (2 lines each)
    let instances_start_y = inner.y + 2;
    for (i, (instance_type, state, avg)) in
        instances_with_stats.iter().take(max_instances).enumerate()
    {
        let y = instances_start_y + (i * 2) as u16;

        if y + 2 > inner.y + inner.height {
            break;
        }

        // Line 1: [status] instance_type progress
        let line1_area = Rect::new(inner.x, y, inner.width, 1);

        // For running instances, use throbber
        if state.status == InstanceStatus::Running {
            // Render throbber in first column
            let throbber_area = Rect::new(inner.x, y, 2, 1);
            let throbber = Throbber::default()
                .throbber_set(BRAILLE_SIX)
                .style(Style::default().fg(t.status_running));

            if let Some(throbber_state) = app.scroll.throbber_states.get_mut(*instance_type) {
                frame.render_stateful_widget(throbber, throbber_area, throbber_state);
            } else {
                frame.render_widget(
                    Paragraph::new("●").style(Style::default().fg(t.status_running)),
                    throbber_area,
                );
            }

            // Rest of line
            let text_area = Rect::new(inner.x + 2, y, inner.width.saturating_sub(2), 1);
            let line1 = Line::from(vec![
                Span::styled(truncate_str(instance_type, 12), t.text()),
                Span::styled(
                    format!(" {}/{}", state.run_progress, state.total_runs),
                    t.dim(),
                ),
            ]);
            frame.render_widget(Paragraph::new(line1), text_area);
        } else {
            // Static status symbol
            let symbol = status_symbol(state.status);
            let color = status_color(state.status);

            let line1 = Line::from(vec![
                Span::styled(format!("{} ", symbol), Style::default().fg(color)),
                Span::styled(truncate_str(instance_type, 12), t.text()),
                Span::styled(
                    format!(" {}/{}", state.run_progress, state.total_runs),
                    t.dim(),
                ),
            ]);
            frame.render_widget(Paragraph::new(line1), line1_area);
        }

        // Line 2: min/avg/max durations
        let line2_area = Rect::new(inner.x + 1, y + 1, inner.width.saturating_sub(1), 1);
        let line2 = if state.durations.is_empty() {
            Line::from(vec![Span::styled("  -", t.dim())])
        } else {
            let min = state
                .durations
                .iter()
                .cloned()
                .filter(|x| x.is_finite())
                .fold(f64::MAX, f64::min);
            let max = state
                .durations
                .iter()
                .cloned()
                .filter(|x| x.is_finite())
                .fold(f64::MIN, f64::max);
            Line::from(vec![
                Span::styled(format!("{:.0}", min), t.success_style()),
                Span::styled("/", t.dim()),
                Span::styled(format!("{:.0}", *avg), t.warning_style()),
                Span::styled("/", t.dim()),
                Span::styled(format!("{:.0}s", max), t.error_style()),
            ])
        };
        frame.render_widget(Paragraph::new(line2), line2_area);
    }
}
