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

    /// Re-sort instances by average duration (fastest first)
    ///
    /// Instances with completed runs are sorted by their average duration (ascending).
    /// Instances without any completed runs are placed at the bottom, sorted alphabetically.
    /// The currently selected instance remains selected after sorting.
    pub fn sort_by_average_duration(&mut self) {
        // Remember which instance was selected
        let selected_type = self.order.get(self.selected_index).cloned();

        // Pre-compute averages to avoid borrow issues during sort
        let averages: HashMap<String, Option<f64>> = self
            .data
            .iter()
            .map(|(k, v)| {
                let avg = if v.durations.is_empty() {
                    None
                } else {
                    Some(v.durations.iter().sum::<f64>() / v.durations.len() as f64)
                };
                (k.clone(), avg)
            })
            .collect();

        // Sort: instances with durations first (by avg ascending), then those without (alphabetically)
        self.order.sort_by(|a, b| {
            let avg_a = averages.get(a).and_then(|v| *v);
            let avg_b = averages.get(b).and_then(|v| *v);

            match (avg_a, avg_b) {
                (Some(a_val), Some(b_val)) => a_val
                    .partial_cmp(&b_val)
                    .unwrap_or(std::cmp::Ordering::Equal),
                (Some(_), None) => std::cmp::Ordering::Less, // With durations first
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.cmp(b), // Alphabetical fallback
            }
        });

        // Restore selection to the same instance
        if let Some(selected) = selected_type {
            self.selected_index = self.order.iter().position(|t| t == &selected).unwrap_or(0);
        }
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
    /// Instances for which cleanup has been requested (to avoid duplicates)
    pub cleanup_requested: HashSet<String>,
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
            .field("cleanup_requested", &self.cleanup_requested.len())
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
            cleanup_requested: HashSet::new(),
        }
    }
}
