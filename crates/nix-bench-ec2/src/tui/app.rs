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

/// Initialization phase
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitPhase {
    Starting,
    CreatingBucket,
    UploadingAgents,
    LaunchingInstances,
    WaitingForInstances,
    Running,
    Completed,
    Failed(String),
}

impl InitPhase {
    pub fn message(&self) -> &str {
        match self {
            InitPhase::Starting => "Starting...",
            InitPhase::CreatingBucket => "Creating S3 bucket...",
            InitPhase::UploadingAgents => "Uploading agent binaries...",
            InitPhase::LaunchingInstances => "Launching EC2 instances...",
            InitPhase::WaitingForInstances => "Waiting for instances to start...",
            InitPhase::Running => "Running benchmarks...",
            InitPhase::Completed => "Completed!",
            InitPhase::Failed(msg) => msg,
        }
    }
}

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
    pub show_logs: bool,
    pub init_phase: InitPhase,
    pub run_id: Option<String>,
    pub bucket_name: Option<String>,
}

impl App {
    /// Create a new app in early/loading state
    pub fn new_loading(instance_types: &[String], total_runs: u32) -> Self {
        let now = Instant::now();

        // Create placeholder instances
        let mut instances = HashMap::new();
        let mut instance_order = Vec::new();

        for instance_type in instance_types {
            instance_order.push(instance_type.clone());
            instances.insert(
                instance_type.clone(),
                InstanceState {
                    instance_id: String::new(),
                    instance_type: instance_type.clone(),
                    system: crate::config::detect_system(instance_type).to_string(),
                    status: InstanceStatus::Pending,
                    run_progress: 0,
                    total_runs,
                    durations: Vec::new(),
                    public_ip: None,
                },
            );
        }

        instance_order.sort();

        Self {
            instances,
            selected_index: 0,
            instance_order,
            should_quit: false,
            total_runs,
            start_time: now,
            last_update: now,
            show_help: false,
            show_logs: false,
            init_phase: InitPhase::Starting,
            run_id: None,
            bucket_name: None,
        }
    }

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
            show_logs: false,
            init_phase: InitPhase::Running,
            run_id: None,
            bucket_name: None,
        }
    }

    /// Update init phase
    pub fn set_phase(&mut self, phase: InitPhase) {
        self.init_phase = phase;
    }

    /// Check if we're still initializing
    pub fn is_initializing(&self) -> bool {
        !matches!(self.init_phase, InitPhase::Running | InitPhase::Completed | InitPhase::Failed(_))
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
        if self.show_help {
            self.show_logs = false;
        }
    }

    /// Toggle logs display
    pub fn toggle_logs(&mut self) {
        self.show_logs = !self.show_logs;
        if self.show_logs {
            self.show_help = false;
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
        let (_tx, mut rx) = tokio::sync::mpsc::channel(1);
        self.run_with_channel(terminal, cloudwatch, config, &mut rx).await
    }

    /// Main event loop with channel for receiving updates
    pub async fn run_with_channel<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        cloudwatch: &CloudWatchClient,
        config: &RunConfig,
        rx: &mut tokio::sync::mpsc::Receiver<crate::tui::TuiMessage>,
    ) -> Result<()> {
        use crate::tui::TuiMessage;

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

                // Receive messages from orchestrator
                Some(msg) = rx.recv() => {
                    match msg {
                        TuiMessage::Phase(phase) => {
                            self.init_phase = phase;
                        }
                        TuiMessage::RunInfo { run_id, bucket_name } => {
                            self.run_id = Some(run_id);
                            self.bucket_name = Some(bucket_name);
                        }
                        TuiMessage::InstanceUpdate { instance_type, instance_id, status, public_ip } => {
                            if let Some(state) = self.instances.get_mut(&instance_type) {
                                state.instance_id = instance_id;
                                state.status = status;
                                state.public_ip = public_ip;
                            }
                        }
                    }
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
                                    KeyCode::Char('l') => {
                                        self.toggle_logs();
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
                    // Check if all complete (only when running)
                    if matches!(self.init_phase, InitPhase::Running) && self.all_complete() {
                        self.init_phase = InitPhase::Completed;
                        // Give user a moment to see final state
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        self.should_quit = true;
                    }
                }

                // Render UI
                _ = render_interval.tick() => {
                    // Move log events from hot buffer to main buffer
                    tui_logger::move_events();
                    terminal.draw(|f| ui::render(f, self))?;
                }

                // Poll CloudWatch (only when running)
                _ = cloudwatch_interval.tick() => {
                    if matches!(self.init_phase, InitPhase::Running) {
                        if let Ok(metrics) = cloudwatch.poll_metrics(&config.instance_types).await {
                            self.update_from_metrics(&metrics);
                        }
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
