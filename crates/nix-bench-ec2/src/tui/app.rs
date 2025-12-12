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
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Application state
pub struct App {
    pub instances: HashMap<String, InstanceState>,
    pub selected_index: usize,
    pub instance_order: Vec<String>,
    pub should_quit: bool,
    pub total_runs: u32,
}

impl App {
    pub fn new(instances: HashMap<String, InstanceState>, total_runs: u32) -> Self {
        let mut instance_order: Vec<String> = instances.keys().cloned().collect();
        instance_order.sort();

        Self {
            instances,
            selected_index: 0,
            instance_order,
            should_quit: false,
            total_runs,
        }
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

    pub fn update_from_metrics(&mut self, metrics: &HashMap<String, crate::aws::cloudwatch::InstanceMetrics>) {
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
    }

    /// Main event loop
    pub async fn run<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        cloudwatch: &CloudWatchClient,
        config: &RunConfig,
    ) -> Result<()> {
        let cancel = CancellationToken::new();

        let mut event_stream = crossterm::event::EventStream::new();
        let mut tick_interval = tokio::time::interval(Duration::from_millis(100));
        let mut render_interval = tokio::time::interval(Duration::from_millis(33));
        let mut cloudwatch_interval = tokio::time::interval(Duration::from_secs(5));

        loop {
            tokio::select! {
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
