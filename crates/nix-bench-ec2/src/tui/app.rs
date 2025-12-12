//! TUI application state and main loop

use crate::aws::CloudWatchClient;
use crate::config::RunConfig;
use crate::orchestrator::{InstanceState, InstanceStatus};
use crate::tui::ui;
use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEventKind};
use futures::StreamExt;
use ratatui::prelude::*;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Application state
pub struct App {
    pub instances: HashMap<String, InstanceState>,
    pub selected_index: usize,
    pub instance_order: Vec<String>,
    pub should_quit: bool,
    pub total_runs: u32,
    pub start_time: Instant,
    pub last_update: Instant,
    pub show_help: bool,
}

impl App {
    pub fn new(instances: HashMap<String, InstanceState>, total_runs: u32) -> Self {
        let mut instance_order: Vec<String> = instances.keys().cloned().collect();
        instance_order.sort();

        let now = Instant::now();
        Self {
            instances,
            selected_index: 0,
            instance_order,
            should_quit: false,
            total_runs,
            start_time: now,
            last_update: now,
            show_help: false,
        }
    }

    /// Get elapsed time since start
    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Format elapsed time as HH:MM:SS
    pub fn elapsed_str(&self) -> String {
        let secs = self.elapsed().as_secs();
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        let secs = secs % 60;
        format!("{:02}:{:02}:{:02}", hours, mins, secs)
    }

    /// Calculate completion percentage
    pub fn completion_percentage(&self) -> f64 {
        let total: u32 = self.instances.len() as u32 * self.total_runs;
        let completed: u32 = self.instances.values().map(|s| s.run_progress).sum();
        if total == 0 {
            0.0
        } else {
            (completed as f64 / total as f64) * 100.0
        }
    }

    /// Estimate remaining time based on current progress
    pub fn estimated_remaining(&self) -> Option<Duration> {
        let completion = self.completion_percentage();
        if completion <= 0.0 || completion >= 100.0 {
            return None;
        }

        let elapsed = self.elapsed().as_secs_f64();
        let total_estimated = elapsed / (completion / 100.0);
        let remaining = total_estimated - elapsed;

        if remaining > 0.0 {
            Some(Duration::from_secs_f64(remaining))
        } else {
            None
        }
    }

    /// Format estimated remaining as string
    pub fn estimated_remaining_str(&self) -> String {
        match self.estimated_remaining() {
            Some(d) => {
                let secs = d.as_secs();
                let mins = secs / 60;
                let secs = secs % 60;
                if mins > 0 {
                    format!("~{}m {}s", mins, secs)
                } else {
                    format!("~{}s", secs)
                }
            }
            None => "-".to_string(),
        }
    }

    /// Toggle help display
    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    pub fn selected_instance(&self) -> Option<&InstanceState> {
        self.instance_order
            .get(self.selected_index)
            .and_then(|k| self.instances.get(k))
    }

    pub fn select_next(&mut self) {
        if self.selected_index < self.instance_order.len().saturating_sub(1) {
            self.selected_index += 1;
        }
    }

    pub fn select_previous(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    pub fn all_complete(&self) -> bool {
        self.instances.values().all(|s| {
            s.status == InstanceStatus::Complete || s.status == InstanceStatus::Failed
        })
    }

    pub fn update_from_metrics(
        &mut self,
        metrics: &HashMap<String, crate::aws::cloudwatch::InstanceMetrics>,
    ) {
        for (instance_type, m) in metrics {
            if let Some(state) = self.instances.get_mut(instance_type) {
                if let Some(status) = m.status {
                    match status {
                        2 => state.status = InstanceStatus::Complete,
                        -1 => state.status = InstanceStatus::Failed,
                        1 => state.status = InstanceStatus::Running,
                        _ => {}
                    }
                }
                if let Some(progress) = m.run_progress {
                    state.run_progress = progress;
                }
                state.durations = m.durations.clone();
            }
        }
        self.last_update = Instant::now();
    }

    /// Main event loop
    pub async fn run<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        cloudwatch: &CloudWatchClient,
        config: &RunConfig,
    ) -> Result<()> {
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        // Set up Ctrl+C handler
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                cancel_clone.cancel();
            }
        });

        let mut event_stream = crossterm::event::EventStream::new();
        let mut tick_interval = tokio::time::interval(Duration::from_millis(100));
        let mut render_interval = tokio::time::interval(Duration::from_millis(33));
        let mut cloudwatch_interval = tokio::time::interval(Duration::from_secs(5));

        loop {
            tokio::select! {
                // Check for cancellation (Ctrl+C)
                _ = cancel.cancelled() => {
                    self.should_quit = true;
                }

                // Handle terminal events
                maybe_event = event_stream.next() => {
                    if let Some(Ok(event)) = maybe_event {
                        if let Event::Key(key) = event {
                            if key.kind == KeyEventKind::Press {
                                match key.code {
                                    KeyCode::Char('q') | KeyCode::Esc => {
                                        self.should_quit = true;
                                    }
                                    KeyCode::Up | KeyCode::Char('k') => {
                                        self.select_previous();
                                    }
                                    KeyCode::Down | KeyCode::Char('j') => {
                                        self.select_next();
                                    }
                                    KeyCode::Char('?') | KeyCode::F(1) => {
                                        self.toggle_help();
                                    }
                                    KeyCode::Home => {
                                        self.selected_index = 0;
                                    }
                                    KeyCode::End => {
                                        self.selected_index = self.instance_order.len().saturating_sub(1);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }

                // Tick for app logic
                _ = tick_interval.tick() => {
                    // Check if all complete
                    if self.all_complete() {
                        // Give user a moment to see final state
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        self.should_quit = true;
                    }
                }

                // Render UI
                _ = render_interval.tick() => {
                    terminal.draw(|f| ui::render(f, self))?;
                }

                // Poll CloudWatch
                _ = cloudwatch_interval.tick() => {
                    if let Ok(metrics) = cloudwatch.poll_metrics(&config.instance_types).await {
                        self.update_from_metrics(&metrics);
                    }
                }
            }

            if self.should_quit {
                break;
            }
        }

        Ok(())
    }
}
