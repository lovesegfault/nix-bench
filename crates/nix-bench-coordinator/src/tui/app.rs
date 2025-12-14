//! TUI application state and main loop

use crate::aws::{GrpcInstanceStatus, GrpcStatusPoller};

/// Default gRPC port for agent communication
const GRPC_PORT: u16 = 50051;
use crate::config::RunConfig;
use crate::orchestrator::{InstanceState, InstanceStatus};
use crate::tui::ui;
use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use futures::StreamExt;
use ratatui::prelude::*;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use throbber_widgets_tui::ThrobberState;
use tokio_util::sync::CancellationToken;
use tui_logger::TuiWidgetState;
use tui_scrollview::ScrollViewState;

/// Default maximum number of lines to keep in the log buffer
const DEFAULT_LOG_BUFFER_MAX_LINES: usize = 10_000;

/// A ring buffer for log lines that caps memory usage by limiting line count.
/// Once the buffer is full, oldest lines are dropped to make room for new ones.
#[derive(Debug, Clone)]
pub struct LogBuffer {
    lines: VecDeque<String>,
    max_lines: usize,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_LOG_BUFFER_MAX_LINES)
    }
}

impl LogBuffer {
    /// Create a new log buffer with the specified maximum line count.
    pub fn new(max_lines: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            max_lines,
        }
    }

    /// Push a single line to the buffer. If at capacity, drops the oldest line.
    pub fn push_line(&mut self, line: String) {
        // Don't add anything if max_lines is 0
        if self.max_lines == 0 {
            return;
        }
        if self.lines.len() >= self.max_lines {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    /// Push multiple lines (from splitting on newlines) to the buffer.
    pub fn push_lines(&mut self, text: &str) {
        for line in text.lines() {
            self.push_line(line.to_string());
        }
    }

    /// Replace all content with new text (splits on newlines).
    pub fn replace(&mut self, text: &str) {
        self.lines.clear();
        self.push_lines(text);
    }

    /// Get the current line count.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Check if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Iterate over lines.
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(|s| s.as_str())
    }

    /// Join all lines with newlines for rendering.
    pub fn as_string(&self) -> String {
        let total_len: usize = self.lines.iter().map(|s| s.len() + 1).sum();
        let mut result = String::with_capacity(total_len);
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                result.push('\n');
            }
            result.push_str(line);
        }
        result
    }
}

/// Initialization phase
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitPhase {
    Starting,
    CreatingBucket,
    CreatingIamRole,
    UploadingAgents,
    LaunchingInstances,
    WaitingForInstances,
    Running,
    CleaningUp(CleanupProgress),
    Completed,
    Failed(String),
}

/// Progress tracking for cleanup phase
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CleanupProgress {
    /// EC2 instances: (completed, total)
    pub ec2_instances: (usize, usize),
    /// S3 bucket deleted
    pub s3_bucket: bool,
    /// IAM roles: (completed, total)
    pub iam_roles: (usize, usize),
    /// Security group rules: (completed, total)
    pub security_rules: (usize, usize),
    /// Current step description
    pub current_step: String,
}

impl CleanupProgress {
    /// Create a new cleanup progress tracker with initial totals
    pub fn new(
        ec2_total: usize,
        _eip_total: usize, // Kept for API compatibility, but unused
        iam_total: usize,
        sg_total: usize,
    ) -> Self {
        Self {
            ec2_instances: (0, ec2_total),
            s3_bucket: false,
            iam_roles: (0, iam_total),
            security_rules: (0, sg_total),
            current_step: "Starting cleanup...".to_string(),
        }
    }

    /// Calculate overall completion percentage
    pub fn percentage(&self) -> f64 {
        let total_items = self.ec2_instances.1
            + 1 // S3 bucket
            + self.iam_roles.1
            + self.security_rules.1;
        if total_items == 0 {
            return 100.0;
        }
        let completed = self.ec2_instances.0
            + (if self.s3_bucket { 1 } else { 0 })
            + self.iam_roles.0
            + self.security_rules.0;
        (completed as f64 / total_items as f64) * 100.0
    }
}

impl InitPhase {
    pub fn message(&self) -> &str {
        match self {
            InitPhase::Starting => "Starting...",
            InitPhase::CreatingBucket => "Creating S3 bucket...",
            InitPhase::CreatingIamRole => "Creating IAM role...",
            InitPhase::UploadingAgents => "Uploading agent binaries...",
            InitPhase::LaunchingInstances => "Launching EC2 instances...",
            InitPhase::WaitingForInstances => "Waiting for instances to start...",
            InitPhase::Running => "Running benchmarks...",
            InitPhase::CleaningUp(progress) => &progress.current_step,
            InitPhase::Completed => "Completed!",
            InitPhase::Failed(msg) => msg,
        }
    }
}

/// Which panel currently has keyboard focus
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PanelFocus {
    #[default]
    InstanceList,
    BuildOutput,
}

// ============================================================================
// App Sub-Structs - Decomposed state for better organization
// ============================================================================

/// Instance-related state
#[derive(Debug, Default)]
pub struct InstancesState {
    /// Map of instance_type to state
    pub data: HashMap<String, InstanceState>,
    /// Ordered list of instance types for display
    pub order: Vec<String>,
    /// Currently selected instance index
    pub selected_index: usize,
}

impl InstancesState {
    /// Get the currently selected instance
    pub fn selected(&self) -> Option<&InstanceState> {
        self.order
            .get(self.selected_index)
            .and_then(|key| self.data.get(key))
    }

    /// Get the currently selected instance (mutable)
    pub fn selected_mut(&mut self) -> Option<&mut InstanceState> {
        self.order
            .get(self.selected_index)
            .cloned()
            .and_then(move |key| self.data.get_mut(&key))
    }

    /// Get the currently selected instance type
    pub fn selected_key(&self) -> Option<&String> {
        self.order.get(self.selected_index)
    }
}

/// UI display state
#[derive(Debug, Default)]
pub struct UiState {
    /// Which panel has keyboard focus
    pub focus: PanelFocus,
    /// Whether to show help popup
    pub show_help: bool,
    /// Whether tracing logs are fullscreen
    pub logs_fullscreen: bool,
    /// Whether to show quit confirmation dialog
    pub show_quit_confirm: bool,
    /// Area where the instance list is rendered (for mouse clicks)
    pub instance_list_area: Option<Rect>,
    /// Scroll offset of the instance list (for mouse click calculation)
    pub list_scroll_offset: usize,
    /// Area where the build output is rendered (for mouse clicks/scroll)
    pub build_output_area: Option<Rect>,
}

/// Scroll and animation state
#[derive(Default)]
pub struct ScrollState {
    /// Scroll state per instance (keyed by instance_type)
    pub log_scroll_states: HashMap<String, ScrollViewState>,
    /// Whether to auto-follow (scroll to bottom) for build output
    pub log_auto_follow: bool,
    /// Throbber state per running instance (keyed by instance_type)
    pub throbber_states: HashMap<String, ThrobberState>,
    /// State for the tracing logs widget
    pub tracing_log_state: TuiWidgetState,
}

impl std::fmt::Debug for ScrollState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScrollState")
            .field("log_scroll_states", &self.log_scroll_states.len())
            .field("log_auto_follow", &self.log_auto_follow)
            .field("throbber_states", &self.throbber_states.len())
            .field("tracing_log_state", &"<TuiWidgetState>")
            .finish()
    }
}

impl ScrollState {
    /// Create with auto-follow enabled
    pub fn new() -> Self {
        Self {
            log_auto_follow: true,
            tracing_log_state: TuiWidgetState::new(),
            ..Default::default()
        }
    }
}

/// Run context and timing
pub struct RunContext {
    /// Unique run identifier
    pub run_id: Option<String>,
    /// S3 bucket name for results
    pub bucket_name: Option<String>,
    /// AWS account ID
    pub aws_account_id: Option<String>,
    /// Total runs per instance
    pub total_runs: u32,
    /// When the run started
    pub start_time: Instant,
    /// Last update time
    pub last_update: Instant,
    /// When all instances completed
    pub completion_time: Option<Instant>,
    /// TLS configuration for gRPC status polling
    pub tls_config: Option<nix_bench_common::TlsConfig>,
}

impl std::fmt::Debug for RunContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunContext")
            .field("run_id", &self.run_id)
            .field("bucket_name", &self.bucket_name)
            .field("aws_account_id", &self.aws_account_id)
            .field("total_runs", &self.total_runs)
            .field("start_time", &self.start_time)
            .field("last_update", &self.last_update)
            .field("completion_time", &self.completion_time)
            .field("tls_config", &self.tls_config.is_some())
            .finish()
    }
}

impl RunContext {
    /// Create a new run context
    pub fn new(total_runs: u32, tls_config: Option<nix_bench_common::TlsConfig>) -> Self {
        let now = Instant::now();
        Self {
            run_id: None,
            bucket_name: None,
            aws_account_id: None,
            total_runs,
            start_time: now,
            last_update: now,
            completion_time: None,
            tls_config,
        }
    }
}

/// Lifecycle state
#[derive(Debug)]
pub struct LifecycleState {
    /// Whether the app should quit
    pub should_quit: bool,
    /// Current initialization phase
    pub init_phase: InitPhase,
}

impl Default for LifecycleState {
    fn default() -> Self {
        Self {
            should_quit: false,
            init_phase: InitPhase::Starting,
        }
    }
}

// ============================================================================
// Main App struct using sub-structs
// ============================================================================

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
    // Convenience accessors for backward compatibility
    // ========================================================================

    /// Get selected index (convenience accessor)
    #[inline]
    pub fn selected_index(&self) -> usize {
        self.instances.selected_index
    }

    /// Get instance order (convenience accessor)
    #[inline]
    pub fn instance_order(&self) -> &[String] {
        &self.instances.order
    }

    /// Get total runs (convenience accessor)
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
            .or_insert_with(ScrollViewState::default);
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
                // Disable auto-follow when user manually scrolls
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
                    .or_insert_with(ThrobberState::default);
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

    pub fn all_complete(&self) -> bool {
        self.instances
            .data
            .values()
            .all(|s| s.status == InstanceStatus::Complete || s.status == InstanceStatus::Failed)
    }

    /// Update instance states from gRPC status polling results
    pub fn update_from_grpc_status(&mut self, status_map: &HashMap<String, GrpcInstanceStatus>) {
        use nix_bench_common::StatusCode;
        for (instance_type, status) in status_map {
            if let Some(state) = self.instances.data.get_mut(instance_type) {
                if let Some(status_code) = status.status {
                    // Use StatusCode enum for conversion
                    if let Some(code) = StatusCode::from_i32(status_code) {
                        state.status = match code {
                            StatusCode::Complete => InstanceStatus::Complete,
                            StatusCode::Failed => InstanceStatus::Failed,
                            StatusCode::Running => InstanceStatus::Running,
                            StatusCode::Pending => InstanceStatus::Pending,
                        };
                    }
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
            None => return, // Can't poll without TLS
        };
        let poller = GrpcStatusPoller::new(&instances_with_ips, GRPC_PORT, tls_config);
        let status_map = poller.poll_status().await;
        if !status_map.is_empty() {
            self.update_from_grpc_status(&status_map);
        }
    }

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
    ///
    /// The `cancel` token is shared with the orchestrator - triggering it will
    /// signal any background tasks to abort.
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
                                // Replace all content (used for full refresh)
                                state.console_output.replace(&output);
                            }
                        }
                        TuiMessage::ConsoleOutputAppend { instance_type, line } => {
                            if let Some(state) = self.instances.data.get_mut(&instance_type) {
                                // Append single line using ring buffer
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
                                // Delegate keyboard handling to the KeyHandler
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
                    // Advance throbber animations
                    self.tick_throbbers();

                    // Check if all complete (only when running)
                    if matches!(self.lifecycle.init_phase, InitPhase::Running) && self.all_complete() {
                        self.lifecycle.init_phase = InitPhase::Completed;
                        // Record completion time for delayed quit
                        if self.context.completion_time.is_none() {
                            self.context.completion_time = Some(Instant::now());
                        }
                    }

                    // Check if we should quit after completion delay (non-blocking)
                    if let Some(completed_at) = self.context.completion_time {
                        if completed_at.elapsed() >= Duration::from_secs(2) {
                            self.lifecycle.should_quit = true;
                        }
                    }
                }

                // Render UI
                _ = render_interval.tick() => {
                    // Move log events from hot buffer to main buffer
                    tui_logger::move_events();
                    terminal.draw(|f| ui::render(f, self))?;
                }

                // Poll gRPC GetStatus from agents (only when running)
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

    /// Run the cleanup phase TUI loop - shows progress until cleanup completes
    ///
    /// Note: Cleanup cannot be cancelled (to avoid orphaned resources), but we
    /// still handle keyboard events to show the TUI is responsive.
    pub async fn run_cleanup_phase<B: ratatui::backend::Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        cleanup_handle: tokio::task::JoinHandle<anyhow::Result<()>>,
        mut rx: tokio::sync::mpsc::Receiver<super::TuiMessage>,
    ) -> anyhow::Result<()> {
        use super::TuiMessage;
        use tokio::time::{interval, Duration};

        let mut event_stream = crossterm::event::EventStream::new();
        let mut render_interval = interval(Duration::from_millis(100));
        let mut cleanup_done = false;
        let mut cleanup_result: Option<anyhow::Result<()>> = None;

        // Pin the handle for polling
        tokio::pin!(cleanup_handle);

        loop {
            tokio::select! {
                // Handle keyboard events during cleanup (for responsiveness)
                // Note: We don't actually quit here - cleanup must complete
                maybe_event = event_stream.next() => {
                    if let Some(Ok(Event::Key(key))) = maybe_event {
                        if key.kind == KeyEventKind::Press {
                            // Acknowledge the quit request but don't abort cleanup
                            // (User will see the cleanup phase continue)
                            if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                                // Already cleaning up - just consume the event
                            }
                        }
                    }
                }

                // Process cleanup progress messages
                Some(msg) = rx.recv() => {
                    if let TuiMessage::Phase(phase) = msg {
                        self.lifecycle.init_phase = phase;
                    }
                }

                // Render UI
                _ = render_interval.tick() => {
                    // Move log events from hot buffer to main buffer
                    tui_logger::move_events();
                    terminal.draw(|f| ui::render(f, self))?;

                    // If cleanup is done, do final render and exit
                    if cleanup_done {
                        self.lifecycle.init_phase = InitPhase::Completed;
                        terminal.draw(|f| ui::render(f, self))?;
                        // Small delay to show the completion state
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        return cleanup_result.unwrap_or(Ok(()));
                    }
                }

                // Wait for cleanup to complete
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
    fn test_log_buffer_new() {
        let buf = LogBuffer::new(100);
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn test_log_buffer_default() {
        let buf = LogBuffer::default();
        assert!(buf.is_empty());
        assert_eq!(buf.max_lines, DEFAULT_LOG_BUFFER_MAX_LINES);
    }

    #[test]
    fn test_log_buffer_push_line() {
        let mut buf = LogBuffer::new(100);
        buf.push_line("line 1".to_string());
        buf.push_line("line 2".to_string());

        assert_eq!(buf.len(), 2);
        let lines: Vec<&str> = buf.lines().collect();
        assert_eq!(lines, vec!["line 1", "line 2"]);
    }

    #[test]
    fn test_log_buffer_overflow() {
        let mut buf = LogBuffer::new(3);
        buf.push_line("line 1".to_string());
        buf.push_line("line 2".to_string());
        buf.push_line("line 3".to_string());
        buf.push_line("line 4".to_string());

        // Should have dropped line 1
        assert_eq!(buf.len(), 3);
        let lines: Vec<&str> = buf.lines().collect();
        assert_eq!(lines, vec!["line 2", "line 3", "line 4"]);
    }

    #[test]
    fn test_log_buffer_push_lines() {
        let mut buf = LogBuffer::new(100);
        buf.push_lines("line 1\nline 2\nline 3");

        assert_eq!(buf.len(), 3);
        let lines: Vec<&str> = buf.lines().collect();
        assert_eq!(lines, vec!["line 1", "line 2", "line 3"]);
    }

    #[test]
    fn test_log_buffer_replace() {
        let mut buf = LogBuffer::new(100);
        buf.push_line("old line".to_string());
        buf.replace("new line 1\nnew line 2");

        assert_eq!(buf.len(), 2);
        let lines: Vec<&str> = buf.lines().collect();
        assert_eq!(lines, vec!["new line 1", "new line 2"]);
    }

    #[test]
    fn test_log_buffer_as_string() {
        let mut buf = LogBuffer::new(100);
        buf.push_line("line 1".to_string());
        buf.push_line("line 2".to_string());

        assert_eq!(buf.as_string(), "line 1\nline 2");
    }

    #[test]
    fn test_log_buffer_as_string_empty() {
        let buf = LogBuffer::new(100);
        assert_eq!(buf.as_string(), "");
    }

    #[test]
    fn test_log_buffer_max_lines_respected() {
        let mut buf = LogBuffer::new(5);
        for i in 0..100 {
            buf.push_line(format!("line {}", i));
        }

        // Should have exactly 5 lines (the last 5)
        assert_eq!(buf.len(), 5);
        let lines: Vec<&str> = buf.lines().collect();
        assert_eq!(
            lines,
            vec!["line 95", "line 96", "line 97", "line 98", "line 99"]
        );
    }

    #[test]
    fn test_log_buffer_zero_max_lines() {
        let mut buf = LogBuffer::new(0);
        buf.push_line("line 1".to_string());

        // With max_lines=0, no lines should be added
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn test_log_buffer_under_capacity() {
        // Test normal operation when well under capacity
        let mut buf = LogBuffer::new(1000);

        // Add fewer lines than capacity
        for i in 0..50 {
            buf.push_line(format!("line {}", i));
        }

        assert_eq!(buf.len(), 50);

        // Verify all lines are present in order
        let lines: Vec<&str> = buf.lines().collect();
        assert_eq!(lines[0], "line 0");
        assert_eq!(lines[49], "line 49");

        // Verify as_string works correctly
        let s = buf.as_string();
        assert!(s.starts_with("line 0\n"));
        assert!(s.ends_with("line 49"));
    }

    #[test]
    fn test_completion_percentage_zero_instances() {
        // Create app with empty instance list
        let app = App::new_loading(&[], 5, None);
        assert_eq!(app.completion_percentage(), 0.0);
    }

    #[test]
    fn test_completion_percentage_zero_runs() {
        // Create app with instances but zero runs
        let app = App::new_loading(&["m5.large".to_string()], 0, None);
        assert_eq!(app.completion_percentage(), 0.0);
    }

    #[test]
    fn test_completion_percentage_normal() {
        let mut app = App::new_loading(&["m5.large".to_string(), "c5.large".to_string()], 10, None);

        // Simulate some progress
        if let Some(state) = app.instances.data.get_mut("m5.large") {
            state.run_progress = 5;
        }
        if let Some(state) = app.instances.data.get_mut("c5.large") {
            state.run_progress = 3;
        }

        // Total: 2 instances * 10 runs = 20 total runs
        // Completed: 5 + 3 = 8 runs
        // Percentage: 8/20 * 100 = 40%
        assert_eq!(app.completion_percentage(), 40.0);
    }

    #[test]
    fn test_tick_throbbers_adds_running_instance() {
        let mut app = App::new_loading(&["m5.large".to_string()], 5, None);

        // Set instance to running
        if let Some(state) = app.instances.data.get_mut("m5.large") {
            state.status = InstanceStatus::Running;
        }

        // Tick should add throbber state
        app.tick_throbbers();

        assert!(app.scroll.throbber_states.contains_key("m5.large"));
    }

    #[test]
    fn test_tick_throbbers_removes_completed_instance() {
        let mut app = App::new_loading(&["m5.large".to_string()], 5, None);

        // Set instance to running first
        if let Some(state) = app.instances.data.get_mut("m5.large") {
            state.status = InstanceStatus::Running;
        }
        app.tick_throbbers();
        assert!(app.scroll.throbber_states.contains_key("m5.large"));

        // Now set to complete
        if let Some(state) = app.instances.data.get_mut("m5.large") {
            state.status = InstanceStatus::Complete;
        }
        app.tick_throbbers();

        // Throbber state should be removed
        assert!(!app.scroll.throbber_states.contains_key("m5.large"));
    }

    #[test]
    fn test_tick_throbbers_cleanup_orphaned_states() {
        let mut app = App::new_loading(&["m5.large".to_string()], 5, None);

        // Manually insert an orphaned throbber state (for instance that doesn't exist)
        app.scroll
            .throbber_states
            .insert("nonexistent".to_string(), ThrobberState::default());

        // Set instance to running
        if let Some(state) = app.instances.data.get_mut("m5.large") {
            state.status = InstanceStatus::Running;
        }

        app.tick_throbbers();

        // The orphaned state should be removed, only m5.large should remain
        assert!(app.scroll.throbber_states.contains_key("m5.large"));
        assert!(!app.scroll.throbber_states.contains_key("nonexistent"));
    }

    mod proptest_tests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// LogBuffer::push_line should never panic
            #[test]
            fn log_buffer_push_line_never_panics(
                max_lines in 0usize..1000,
                lines in prop::collection::vec(".*", 0..200)
            ) {
                let mut buf = LogBuffer::new(max_lines);
                for line in lines {
                    buf.push_line(line);
                }
            }

            /// LogBuffer should never exceed max_lines
            #[test]
            fn log_buffer_respects_max_lines(
                max_lines in 1usize..100,
                lines in prop::collection::vec(".*", 0..200)
            ) {
                let mut buf = LogBuffer::new(max_lines);
                for line in lines {
                    buf.push_line(line);
                }
                prop_assert!(
                    buf.len() <= max_lines,
                    "Buffer len {} exceeds max_lines {}",
                    buf.len(),
                    max_lines
                );
            }

            /// LogBuffer::push_lines should never panic
            #[test]
            fn log_buffer_push_lines_never_panics(
                max_lines in 0usize..1000,
                text in ".*"
            ) {
                let mut buf = LogBuffer::new(max_lines);
                buf.push_lines(&text);
            }

            /// LogBuffer::replace should never panic
            #[test]
            fn log_buffer_replace_never_panics(
                max_lines in 0usize..1000,
                text1 in ".*",
                text2 in ".*"
            ) {
                let mut buf = LogBuffer::new(max_lines);
                buf.push_lines(&text1);
                buf.replace(&text2);
            }

            /// LogBuffer::as_string should never panic and round-trip correctly
            #[test]
            fn log_buffer_as_string_never_panics(
                max_lines in 1usize..100,
                lines in prop::collection::vec("[^\n\r]*", 0..50)
            ) {
                let mut buf = LogBuffer::new(max_lines);
                for line in &lines {
                    buf.push_line(line.clone());
                }
                let _ = buf.as_string();
                // Verify line count
                let expected_count = lines.len().min(max_lines);
                prop_assert_eq!(buf.len(), expected_count);
            }

            /// Zero max_lines buffer stays empty
            #[test]
            fn log_buffer_zero_max_stays_empty(lines in prop::collection::vec(".*", 0..100)) {
                let mut buf = LogBuffer::new(0);
                for line in lines {
                    buf.push_line(line);
                }
                prop_assert_eq!(buf.len(), 0);
                prop_assert!(buf.is_empty());
            }

            /// LogBuffer preserves newest lines when overflowing
            #[test]
            fn log_buffer_preserves_newest(
                max_lines in 1usize..20,
                num_lines in 1usize..100
            ) {
                let mut buf = LogBuffer::new(max_lines);
                for i in 0..num_lines {
                    buf.push_line(format!("line{}", i));
                }

                // The last line should be the newest
                let lines: Vec<&str> = buf.lines().collect();
                if !lines.is_empty() {
                    let expected_last = format!("line{}", num_lines - 1);
                    prop_assert_eq!(lines.last().unwrap(), &expected_last.as_str());
                }
            }
        }
    }
}
