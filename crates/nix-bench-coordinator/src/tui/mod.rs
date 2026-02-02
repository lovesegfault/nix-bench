//! TUI dashboard for benchmark monitoring

mod app;
pub mod input;
pub mod log_capture;
pub mod theme;
mod ui;
pub mod widgets;

pub use app::{
    App, CleanupProgress, InitPhase, InstancesState, LifecycleState, LogBuffer, PanelFocus,
    ScrollState, UiState,
};
pub use input::{KeyHandler, KeyResult};
pub use log_capture::{LogCapture, LogCaptureLayer};

/// Truncate a string to fit within a maximum display width, adding ellipsis if needed.
pub fn truncate_str(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if s.len() <= max_width {
        return s.to_string();
    }
    // Truncate at a char boundary, leaving room for ellipsis
    let end = s
        .char_indices()
        .take_while(|&(i, _)| i < max_width.saturating_sub(1))
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    format!("{}…", &s[..end])
}

/// Message sent to TUI to update state
///
/// During init, the background task sends phase/account/run messages.
/// When init completes, it sends `InitComplete` with the `RunEngine`.
/// After that, log streaming messages may still arrive via `ConsoleOutputAppend`.
#[derive(Debug)]
pub enum TuiMessage {
    /// Update init phase
    Phase(InitPhase),
    /// Set AWS account info
    AccountInfo { account_id: String },
    /// Background init completed — here is the engine
    InitComplete(Box<crate::orchestrator::RunEngine>),
    /// Update instance state
    InstanceUpdate {
        instance_type: String,
        instance_id: String,
        status: crate::orchestrator::InstanceStatus,
        public_ip: Option<String>,
        run_progress: Option<u32>,
    },
    /// Update console output for an instance (full replacement)
    ConsoleOutput {
        instance_type: String,
        output: String,
    },
    /// Append to console output for an instance (incremental, avoids O(n²) copies)
    ConsoleOutputAppend { instance_type: String, line: String },
}
