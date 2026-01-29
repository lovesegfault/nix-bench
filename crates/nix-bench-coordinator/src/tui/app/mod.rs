//! TUI application state and main loop

mod event_loop;
mod grpc_polling;
mod lifecycle;
mod mouse;
mod state;

pub use crate::log_buffer::LogBuffer;
pub use lifecycle::{CleanupProgress, InitPhase, LifecycleState};
pub use state::{InstancesState, PanelFocus, RunContext, ScrollState, UiState};

use crate::orchestrator::{InstanceState, InstanceStatus};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// Ensures the Ctrl+C signal handler is only spawned once per process.
static CTRLC_HANDLER_INSTALLED: AtomicBool = AtomicBool::new(false);
use throbber_widgets_tui::ThrobberState;
use tui_scrollview::ScrollViewState;

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
                    system: nix_bench_common::Architecture::from_instance_type(instance_type),
                    status: InstanceStatus::Pending,
                    run_progress: 0,
                    total_runs,
                    run_results: Vec::new(),
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
        let total: u32 = (self.instances.data.len() as u32).saturating_mul(self.context.total_runs);
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
        self.instances.data.values().all(|s| {
            matches!(
                s.status,
                InstanceStatus::Complete | InstanceStatus::Failed | InstanceStatus::Terminated
            )
        })
    }

    /// Check if all results have been captured for completed instances.
    ///
    /// This verifies that for each Complete instance, we've received all the
    /// expected duration results (durations.len() >= run_progress). This guards
    /// against the race where we see Complete status before the final duration
    /// is polled.
    pub fn all_results_captured(&self) -> bool {
        self.instances.data.values().all(|s| {
            match s.status {
                // Complete instances must have all their durations
                InstanceStatus::Complete => s.durations().len() as u32 >= s.run_progress,
                // Failed/Terminated instances may not have all results, that's OK
                InstanceStatus::Failed | InstanceStatus::Terminated => true,
                // Still running, not captured yet
                _ => false,
            }
        })
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
        if let Some(key) = self
            .instances
            .order
            .get(self.instances.selected_index)
            .cloned()
        {
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
        if let Some(key) = self
            .instances
            .order
            .get(self.instances.selected_index)
            .cloned()
        {
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
        if let Some(key) = self
            .instances
            .order
            .get(self.instances.selected_index)
            .cloned()
        {
            self.ensure_scroll_state(&key);
            if let Some(state) = self.scroll.log_scroll_states.get_mut(&key) {
                state.scroll_to_top();
                self.scroll.log_auto_follow = false;
            }
        }
    }

    /// Jump to bottom of build output and enable auto-follow
    pub fn scroll_to_bottom(&mut self) {
        if let Some(key) = self
            .instances
            .order
            .get(self.instances.selected_index)
            .cloned()
        {
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

    #[test]
    fn test_all_results_captured_complete_with_all_durations() {
        let mut app = App::new_loading(&["m5.large".to_string()], 5, None);

        if let Some(state) = app.instances.data.get_mut("m5.large") {
            state.status = InstanceStatus::Complete;
            state.run_progress = 5;
            state.run_results = [1.0, 2.0, 3.0, 4.0, 5.0]
                .iter()
                .enumerate()
                .map(|(i, &d)| nix_bench_common::RunResult::success((i + 1) as u32, d))
                .collect();
        }

        assert!(app.all_results_captured());
    }

    #[test]
    fn test_all_results_captured_complete_missing_duration() {
        let mut app = App::new_loading(&["m5.large".to_string()], 5, None);

        if let Some(state) = app.instances.data.get_mut("m5.large") {
            state.status = InstanceStatus::Complete;
            state.run_progress = 5;
            // Only 4 durations captured, but run_progress says 5 completed
            state.run_results = [1.0, 2.0, 3.0, 4.0]
                .iter()
                .enumerate()
                .map(|(i, &d)| nix_bench_common::RunResult::success((i + 1) as u32, d))
                .collect();
        }

        assert!(!app.all_results_captured());
    }

    #[test]
    fn test_all_results_captured_failed_instance() {
        let mut app = App::new_loading(&["m5.large".to_string()], 5, None);

        if let Some(state) = app.instances.data.get_mut("m5.large") {
            state.status = InstanceStatus::Failed;
            state.run_progress = 3;
            // Failed instances don't need all durations
            state.run_results = [1.0, 2.0]
                .iter()
                .enumerate()
                .map(|(i, &d)| nix_bench_common::RunResult::success((i + 1) as u32, d))
                .collect();
        }

        assert!(app.all_results_captured());
    }

    #[test]
    fn test_all_results_captured_running_instance() {
        let mut app = App::new_loading(&["m5.large".to_string()], 5, None);

        if let Some(state) = app.instances.data.get_mut("m5.large") {
            state.status = InstanceStatus::Running;
            state.run_progress = 3;
            state.run_results = [1.0, 2.0, 3.0]
                .iter()
                .enumerate()
                .map(|(i, &d)| nix_bench_common::RunResult::success((i + 1) as u32, d))
                .collect();
        }

        // Running instances are not "captured"
        assert!(!app.all_results_captured());
    }
}
