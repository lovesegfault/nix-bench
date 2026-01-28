//! TUI dashboard for benchmark monitoring

mod app;
pub mod input;
pub mod log_capture;
pub mod theme;
mod ui;
pub mod widgets;

pub use app::{
    App, CleanupProgress, InitPhase, InstancesState, LifecycleState, LogBuffer, PanelFocus,
    RunContext, ScrollState, UiState,
};
pub use input::{KeyHandler, KeyResult};
pub use log_capture::{LogCapture, LogCaptureLayer};

/// Truncate a string to fit within a maximum display width, adding ellipsis if needed.
/// This is unicode-safe and won't panic on multibyte characters.
pub fn truncate_str(s: &str, max_width: usize) -> String {
    use unicode_truncate::UnicodeTruncateStr;
    use unicode_width::UnicodeWidthStr;

    if max_width == 0 {
        return String::new();
    }

    // Use width() directly instead of unicode_truncate(usize::MAX) for efficiency
    if s.width() <= max_width {
        return s.to_string();
    }

    // Need to truncate: leave room for ellipsis
    let (truncated, _) = s.unicode_truncate(max_width.saturating_sub(1));
    format!("{}…", truncated)
}

/// Message sent to TUI to update state
#[derive(Debug, Clone)]
pub enum TuiMessage {
    /// Update init phase
    Phase(InitPhase),
    /// Set AWS account info
    AccountInfo { account_id: String },
    /// Set run ID and bucket name
    RunInfo { run_id: String, bucket_name: String },
    /// Set TLS configuration for gRPC status polling
    TlsConfig { config: nix_bench_common::TlsConfig },
    /// Update instance state
    InstanceUpdate {
        instance_type: String,
        instance_id: String,
        status: crate::orchestrator::InstanceStatus,
        public_ip: Option<String>,
        run_progress: Option<u32>,
        durations: Option<Vec<f64>>,
    },
    /// Update console output for an instance (full replacement)
    ConsoleOutput {
        instance_type: String,
        output: String,
    },
    /// Append to console output for an instance (incremental, avoids O(n²) copies)
    ConsoleOutputAppend { instance_type: String, line: String },
}
