//! TUI application state and main loop

mod lifecycle;
mod log_buffer;
mod state;

pub use lifecycle::{CleanupProgress, InitPhase, LifecycleState};
pub use log_buffer::LogBuffer;
pub use state::{InstancesState, PanelFocus, RunContext, ScrollState, UiState};

use crate::aws::{GrpcInstanceStatus, GrpcStatusPoller};
use crate::config::RunConfig;
use crate::orchestrator::{InstanceState, InstanceStatus};
use crate::tui::ui;
use anyhow::Result;
use crossterm::event::{Event, KeyEventKind, MouseButton, MouseEventKind};
use futures::StreamExt;
use ratatui::prelude::*;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use throbber_widgets_tui::ThrobberState;
use tokio_util::sync::CancellationToken;
use tui_scrollview::ScrollViewState;

/// Default gRPC port for agent communication
const GRPC_PORT: u16 = 50051;

/// Application state
///
/// Organized into focused sub-structs:
/// - `instances`: Instance data and selection
/// - `ui`: Display state (focus, popups, areas)
/// - `scroll`: Scroll positions and animations
/// - `context`: Run metadata and timing
/// - `lifecycle`: Quit and phase state
pub struct App {
    /// Instance data and selection state
    pub instances: InstancesState,
    /// UI display state
    pub ui: UiState,
    /// Scroll and animation state
    pub scroll: ScrollState,
    /// Run context and timing
    pub context: RunContext,
    /// Lifecycle state
    pub lifecycle: LifecycleState,
}

impl App {
    /// Create a new app in early/loading state
    pub fn new_loading(
        instance_types: &[String],
        total_runs: u32,
        tls_config: Option<nix_bench_common::TlsConfig>,
    ) -> Self {
        // Create placeholder instances
        let mut data = HashMap::new();
        let mut order = Vec::new();

        for instance_type in instance_types {
            order.push(instance_type.clone());
            data.insert(
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
                    console_output: LogBuffer::default(),
                },
            );
        }

        order.sort();

        Self {
            instances: InstancesState {
                data,
                order,
                selected_index: 0,
            },
            ui: UiState::default(),
            scroll: ScrollState::new(),
            context: RunContext::new(total_runs, tls_config),
            lifecycle: LifecycleState::default(),
        }
    }

    // ========================================================================
    // Convenience accessors
    // ========================================================================

    /// Get selected index
    #[inline]
    pub fn selected_index(&self) -> usize {
        self.instances.selected_index
    }

    /// Get instance order
    #[inline]
    pub fn instance_order(&self) -> &[String] {
        &self.instances.order
    }

    /// Get total runs
    #[inline]
    pub fn total_runs(&self) -> u32 {
        self.context.total_runs
    }

    /// Check if we're still initializing
    pub fn is_initializing(&self) -> bool {
        !matches!(
            self.lifecycle.init_phase,
            InitPhase::Running
                | InitPhase::CleaningUp(_)
                | InitPhase::Completed
                | InitPhase::Failed(_)
        )
    }

    /// Check if we're in the cleanup phase
    pub fn is_cleaning_up(&self) -> bool {
        matches!(self.lifecycle.init_phase, InitPhase::CleaningUp(_))
    }

    /// Get elapsed time since start
    pub fn elapsed(&self) -> Duration {
        self.context.start_time.elapsed()
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
        let total: u32 =
            (self.instances.data.len() as u32).saturating_mul(self.context.total_runs);
        let completed: u32 = self.instances.data.values().map(|s| s.run_progress).sum();
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
        self.ui.show_help = !self.ui.show_help;
    }

    /// Toggle focus between instance list and build output
    pub fn toggle_focus(&mut self) {
        self.ui.focus = match self.ui.focus {
            PanelFocus::InstanceList => PanelFocus::BuildOutput,
            PanelFocus::BuildOutput => PanelFocus::InstanceList,
        };
    }

    pub fn selected_instance(&self) -> Option<&InstanceState> {
        self.instances.selected()
    }

    pub fn select_next(&mut self) {
        if self.instances.selected_index < self.instances.order.len().saturating_sub(1) {
            self.instances.selected_index += 1;
        }
    }

    pub fn select_previous(&mut self) {
        if self.instances.selected_index > 0 {
            self.instances.selected_index -= 1;
        }
    }

    pub fn all_complete(&self) -> bool {
        self.instances
            .data
            .values()
            .all(|s| s.status == InstanceStatus::Complete || s.status == InstanceStatus::Failed)
    }

    // ========================================================================
    // Scroll methods
    // ========================================================================

    /// Get the scroll state for the currently selected instance
    pub fn current_scroll_state(&mut self) -> Option<&mut ScrollViewState> {
        self.instances
            .order
            .get(self.instances.selected_index)
            .and_then(|key| self.scroll.log_scroll_states.get_mut(key))
    }

    /// Ensure scroll state exists for an instance
    pub fn ensure_scroll_state(&mut self, instance_type: &str) {
        self.scroll
            .log_scroll_states
            .entry(instance_type.to_string())
            .or_default();
    }

    /// Scroll build output up by n lines
    pub fn scroll_up(&mut self, lines: u16) {
        if let Some(key) = self.instances.order.get(self.instances.selected_index).cloned() {
            self.ensure_scroll_state(&key);
            if let Some(state) = self.scroll.log_scroll_states.get_mut(&key) {
                for _ in 0..lines {
                    state.scroll_up();
                }
                self.scroll.log_auto_follow = false;
            }
        }
    }

    /// Scroll build output down by n lines
    pub fn scroll_down(&mut self, lines: u16) {
        if let Some(key) = self.instances.order.get(self.instances.selected_index).cloned() {
            self.ensure_scroll_state(&key);
            if let Some(state) = self.scroll.log_scroll_states.get_mut(&key) {
                for _ in 0..lines {
                    state.scroll_down();
                }
                self.scroll.log_auto_follow = false;
            }
        }
    }

    /// Jump to top of build output
    pub fn scroll_to_top(&mut self) {
        if let Some(key) = self.instances.order.get(self.instances.selected_index).cloned() {
            self.ensure_scroll_state(&key);
            if let Some(state) = self.scroll.log_scroll_states.get_mut(&key) {
                state.scroll_to_top();
                self.scroll.log_auto_follow = false;
            }
        }
    }

    /// Jump to bottom of build output and enable auto-follow
    pub fn scroll_to_bottom(&mut self) {
        if let Some(key) = self.instances.order.get(self.instances.selected_index).cloned() {
            self.ensure_scroll_state(&key);
            if let Some(state) = self.scroll.log_scroll_states.get_mut(&key) {
                state.scroll_to_bottom();
                self.scroll.log_auto_follow = true;
            }
        }
    }

    /// Advance all throbber animations (call on tick)
    pub fn tick_throbbers(&mut self) {
        // Add throbber states for running instances
        for (instance_type, state) in &self.instances.data {
            if state.status == InstanceStatus::Running {
                self.scroll
                    .throbber_states
                    .entry(instance_type.clone())
                    .or_default();
            }
        }

        // Advance all throbber states
        for state in self.scroll.throbber_states.values_mut() {
            state.calc_next();
        }

        // Remove throbber states for non-running instances
        self.scroll.throbber_states.retain(|instance_type, _| {
            self.instances
                .data
                .get(instance_type)
                .map(|s| s.status == InstanceStatus::Running)
                .unwrap_or(false)
        });

        // Clean up scroll states for removed instances
        self.scroll
            .log_scroll_states
            .retain(|k, _| self.instances.data.contains_key(k));
    }

    /// Get throbber state for an instance (if running)
    pub fn get_throbber_state(&mut self, instance_type: &str) -> Option<&mut ThrobberState> {
        self.scroll.throbber_states.get_mut(instance_type)
    }

    // ========================================================================
    // Mouse handling
    // ========================================================================

    /// Handle a mouse click at the given position
    /// Returns true if the click was handled
    pub fn handle_mouse_click(&mut self, x: u16, y: u16) -> bool {
        // Check instance list area
        if let Some(area) = self.ui.instance_list_area {
            let content_x = area.x + 1;
            let content_y = area.y + 1;
            let content_width = area.width.saturating_sub(2);
            let content_height = area.height.saturating_sub(2);

            if x >= content_x
                && x < content_x + content_width
                && y >= content_y
                && y < content_y + content_height
            {
                // Account for list scroll offset when calculating clicked index
                let clicked_index = self.ui.list_scroll_offset + (y - content_y) as usize;

                if clicked_index < self.instances.order.len() {
                    self.instances.selected_index = clicked_index;
                    self.ui.focus = PanelFocus::InstanceList;
                    return true;
                }
            }
        }

        // Check build output area
        if let Some(area) = self.ui.build_output_area {
            if x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height {
                self.ui.focus = PanelFocus::BuildOutput;
                return true;
            }
        }

        false
    }

    /// Handle mouse scroll at given position
    pub fn handle_mouse_scroll(&mut self, x: u16, y: u16, down: bool) {
        // Check if scroll is in build output area
        if let Some(area) = self.ui.build_output_area {
            if x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height {
                if down {
                    self.scroll_down(3);
                } else {
                    self.scroll_up(3);
                }
                return;
            }
        }

        // Default: scroll instance list
        if down {
            self.select_next();
        } else {
            self.select_previous();
        }
    }

    // ========================================================================
    // gRPC status polling
    // ========================================================================

    /// Update instance states from gRPC status polling results
    pub fn update_from_grpc_status(&mut self, status_map: &HashMap<String, GrpcInstanceStatus>) {
        use nix_bench_common::StatusCode;
        for (instance_type, status) in status_map {
            if let Some(state) = self.instances.data.get_mut(instance_type) {
                if let Some(status_code) = status.status {
                    state.status = match status_code {
                        StatusCode::Complete => InstanceStatus::Complete,
                        StatusCode::Failed => InstanceStatus::Failed,
                        StatusCode::Running | StatusCode::Bootstrap | StatusCode::Warmup => InstanceStatus::Running,
                        StatusCode::Pending => InstanceStatus::Pending,
                    };
                }
                if let Some(progress) = status.run_progress {
                    state.run_progress = progress;
                }
                state.durations = status.durations.clone();
            }
        }
        self.context.last_update = Instant::now();
    }

    /// Get instances that have public IPs (for gRPC polling)
    fn get_instances_with_ips(&self) -> Vec<(String, String)> {
        self.instances
            .data
            .iter()
            .filter_map(|(instance_type, state)| {
                state
                    .public_ip
                    .as_ref()
                    .map(|ip| (instance_type.clone(), ip.clone()))
            })
            .collect()
    }

    /// Poll gRPC GetStatus from all instances with IPs
    async fn poll_grpc_status(&mut self) {
        let instances_with_ips = self.get_instances_with_ips();
        if instances_with_ips.is_empty() {
            return;
        }

        // TLS is required for gRPC polling - skip if not configured yet
        let tls_config = match &self.context.tls_config {
            Some(tls) => tls.clone(),
            None => return,
        };
        let poller = GrpcStatusPoller::new(&instances_with_ips, GRPC_PORT, tls_config);
        let status_map = poller.poll_status().await;
        if !status_map.is_empty() {
            self.update_from_grpc_status(&status_map);
        }
    }

    // ========================================================================
    // Event loop
    // ========================================================================

    /// Main event loop
    pub async fn run<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        _config: &RunConfig,
    ) -> Result<()> {
        let (_tx, mut rx) = tokio::sync::mpsc::channel(1);
        let cancel = CancellationToken::new();
        self.run_with_channel(terminal, &mut rx, cancel).await
    }

    /// Main event loop with channel for receiving updates
    pub async fn run_with_channel<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        rx: &mut tokio::sync::mpsc::Receiver<crate::tui::TuiMessage>,
        cancel: CancellationToken,
    ) -> Result<()> {
        use crate::tui::TuiMessage;

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
        let mut grpc_poll_interval = tokio::time::interval(Duration::from_millis(500));

        loop {
            tokio::select! {
                // Check for cancellation (Ctrl+C)
                _ = cancel.cancelled() => {
                    self.lifecycle.should_quit = true;
                }

                // Receive messages from orchestrator
                Some(msg) = rx.recv() => {
                    match msg {
                        TuiMessage::Phase(phase) => {
                            self.lifecycle.init_phase = phase;
                        }
                        TuiMessage::AccountInfo { account_id } => {
                            self.context.aws_account_id = Some(account_id);
                        }
                        TuiMessage::RunInfo { run_id, bucket_name } => {
                            self.context.run_id = Some(run_id);
                            self.context.bucket_name = Some(bucket_name);
                        }
                        TuiMessage::TlsConfig { config } => {
                            self.context.tls_config = Some(config);
                        }
                        TuiMessage::InstanceUpdate { instance_type, instance_id, status, public_ip } => {
                            if let Some(state) = self.instances.data.get_mut(&instance_type) {
                                state.instance_id = instance_id;
                                state.status = status;
                                state.public_ip = public_ip;
                            }
                        }
                        TuiMessage::ConsoleOutput { instance_type, output } => {
                            if let Some(state) = self.instances.data.get_mut(&instance_type) {
                                state.console_output.replace(&output);
                            }
                        }
                        TuiMessage::ConsoleOutputAppend { instance_type, line } => {
                            if let Some(state) = self.instances.data.get_mut(&instance_type) {
                                state.console_output.push_line(line);
                            }
                        }
                    }
                }

                // Handle terminal events
                maybe_event = event_stream.next() => {
                    if let Some(Ok(event)) = maybe_event {
                        match event {
                            Event::Key(key) if key.kind == KeyEventKind::Press => {
                                use crate::tui::input::KeyHandler;
                                let _ = KeyHandler::handle(self, key, &cancel);
                            }
                            Event::Mouse(mouse) => {
                                match mouse.kind {
                                    MouseEventKind::Down(MouseButton::Left) => {
                                        self.handle_mouse_click(mouse.column, mouse.row);
                                    }
                                    MouseEventKind::ScrollDown => {
                                        self.handle_mouse_scroll(mouse.column, mouse.row, true);
                                    }
                                    MouseEventKind::ScrollUp => {
                                        self.handle_mouse_scroll(mouse.column, mouse.row, false);
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }
                }

                // Tick for app logic
                _ = tick_interval.tick() => {
                    self.tick_throbbers();

                    if matches!(self.lifecycle.init_phase, InitPhase::Running) && self.all_complete() {
                        self.lifecycle.init_phase = InitPhase::Completed;
                        if self.context.completion_time.is_none() {
                            self.context.completion_time = Some(Instant::now());
                        }
                    }

                    if let Some(completed_at) = self.context.completion_time {
                        if completed_at.elapsed() >= Duration::from_secs(2) {
                            self.lifecycle.should_quit = true;
                        }
                    }
                }

                // Render UI
                _ = render_interval.tick() => {
                    tui_logger::move_events();
                    terminal.draw(|f| ui::render(f, self))?;
                }

                // Poll gRPC GetStatus from agents
                _ = grpc_poll_interval.tick() => {
                    if matches!(self.lifecycle.init_phase, InitPhase::Running) {
                        self.poll_grpc_status().await;
                    }
                }
            }

            if self.lifecycle.should_quit {
                break;
            }
        }

        Ok(())
    }

    /// Run the cleanup phase TUI loop
    pub async fn run_cleanup_phase<B: ratatui::backend::Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        cleanup_handle: tokio::task::JoinHandle<anyhow::Result<()>>,
        mut rx: tokio::sync::mpsc::Receiver<super::TuiMessage>,
    ) -> anyhow::Result<()> {
        use super::TuiMessage;
        use crossterm::event::KeyCode;
        use tokio::time::{interval, Duration};

        let mut event_stream = crossterm::event::EventStream::new();
        let mut render_interval = interval(Duration::from_millis(100));
        let mut cleanup_done = false;
        let mut cleanup_result: Option<anyhow::Result<()>> = None;

        tokio::pin!(cleanup_handle);

        loop {
            tokio::select! {
                maybe_event = event_stream.next() => {
                    if let Some(Ok(Event::Key(key))) = maybe_event {
                        if key.kind == KeyEventKind::Press
                            && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                        {
                            // Already cleaning up - just consume the event
                        }
                    }
                }

                Some(msg) = rx.recv() => {
                    if let TuiMessage::Phase(phase) = msg {
                        self.lifecycle.init_phase = phase;
                    }
                }

                _ = render_interval.tick() => {
                    tui_logger::move_events();
                    terminal.draw(|f| ui::render(f, self))?;

                    if cleanup_done {
                        self.lifecycle.init_phase = InitPhase::Completed;
                        terminal.draw(|f| ui::render(f, self))?;
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        return cleanup_result.unwrap_or(Ok(()));
                    }
                }

                result = &mut cleanup_handle, if !cleanup_done => {
                    cleanup_done = true;
                    cleanup_result = Some(result?);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_completion_percentage_zero_instances() {
        let app = App::new_loading(&[], 5, None);
        assert_eq!(app.completion_percentage(), 0.0);
    }

    #[test]
    fn test_completion_percentage_zero_runs() {
        let app = App::new_loading(&["m5.large".to_string()], 0, None);
        assert_eq!(app.completion_percentage(), 0.0);
    }

    #[test]
    fn test_completion_percentage_normal() {
        let mut app = App::new_loading(&["m5.large".to_string(), "c5.large".to_string()], 10, None);

        if let Some(state) = app.instances.data.get_mut("m5.large") {
            state.run_progress = 5;
        }
        if let Some(state) = app.instances.data.get_mut("c5.large") {
            state.run_progress = 3;
        }

        assert_eq!(app.completion_percentage(), 40.0);
    }

    #[test]
    fn test_tick_throbbers_adds_running_instance() {
        let mut app = App::new_loading(&["m5.large".to_string()], 5, None);

        if let Some(state) = app.instances.data.get_mut("m5.large") {
            state.status = InstanceStatus::Running;
        }

        app.tick_throbbers();
        assert!(app.scroll.throbber_states.contains_key("m5.large"));
    }

    #[test]
    fn test_tick_throbbers_removes_completed_instance() {
        let mut app = App::new_loading(&["m5.large".to_string()], 5, None);

        if let Some(state) = app.instances.data.get_mut("m5.large") {
            state.status = InstanceStatus::Running;
        }
        app.tick_throbbers();
        assert!(app.scroll.throbber_states.contains_key("m5.large"));

        if let Some(state) = app.instances.data.get_mut("m5.large") {
            state.status = InstanceStatus::Complete;
        }
        app.tick_throbbers();
        assert!(!app.scroll.throbber_states.contains_key("m5.large"));
    }

    #[test]
    fn test_tick_throbbers_cleanup_orphaned_states() {
        let mut app = App::new_loading(&["m5.large".to_string()], 5, None);

        app.scroll
            .throbber_states
            .insert("nonexistent".to_string(), ThrobberState::default());

        if let Some(state) = app.instances.data.get_mut("m5.large") {
            state.status = InstanceStatus::Running;
        }

        app.tick_throbbers();

        assert!(app.scroll.throbber_states.contains_key("m5.large"));
        assert!(!app.scroll.throbber_states.contains_key("nonexistent"));
    }
}
