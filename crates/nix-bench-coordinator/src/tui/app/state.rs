//! Application state types

use crate::orchestrator::InstanceState;
use ratatui::prelude::Rect;
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use throbber_widgets_tui::ThrobberState;
use tui_logger::TuiWidgetState;
use tui_scrollview::ScrollViewState;

/// Which panel currently has keyboard focus
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PanelFocus {
    #[default]
    InstanceList,
    BuildOutput,
}

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
    /// Instances for which termination has been requested (to avoid duplicates)
    pub termination_requested: HashSet<String>,
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
            .field("termination_requested", &self.termination_requested.len())
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
            termination_requested: HashSet::new(),
        }
    }
}
