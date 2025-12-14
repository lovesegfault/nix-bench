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
    },
    /// Update console output for an instance (full replacement)
    ConsoleOutput {
        instance_type: String,
        output: String,
    },
    /// Append to console output for an instance (incremental, avoids O(n²) copies)
    ConsoleOutputAppend {
        instance_type: String,
        line: String,
    },
}

#[cfg(test)]
mod tests {
    use super::truncate_str;

    #[test]
    fn test_truncate_str_ascii() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello", 5), "hello");
        assert_eq!(truncate_str("hello world", 5), "hell…");
    }

    #[test]
    fn test_truncate_str_zero_width() {
        assert_eq!(truncate_str("hello", 0), "");
        assert_eq!(truncate_str("", 0), "");
    }

    #[test]
    fn test_truncate_str_empty() {
        assert_eq!(truncate_str("", 10), "");
    }

    #[test]
    fn test_truncate_str_emoji() {
        // Emoji are typically 2 display columns wide
        assert_eq!(truncate_str("🦀", 10), "🦀");
        assert_eq!(truncate_str("🦀🦀🦀", 5), "🦀🦀…"); // 2+2 = 4, then ellipsis
        // Width 2 can't fit a crab (width 2) + ellipsis (width 1), so we get just ellipsis
        assert_eq!(truncate_str("🦀🦀🦀", 2), "…");
        // Width 3 can fit one crab + ellipsis
        assert_eq!(truncate_str("🦀🦀🦀", 3), "🦀…");
    }

    #[test]
    fn test_truncate_str_cjk() {
        // CJK characters are typically 2 display columns wide
        assert_eq!(truncate_str("日本語", 10), "日本語"); // Fits (6 cols)
        assert_eq!(truncate_str("日本語テスト", 6), "日本…"); // Truncate to 5 + ellipsis
    }

    #[test]
    fn test_truncate_str_mixed() {
        // Mix of ASCII and wide characters
        assert_eq!(truncate_str("ab日c", 10), "ab日c"); // Fits (4 cols)
        assert_eq!(truncate_str("ab日本語", 5), "ab日…"); // Truncate
    }

    #[test]
    fn test_truncate_str_exact_fit() {
        // String that fits exactly
        assert_eq!(truncate_str("hello", 5), "hello");
        assert_eq!(truncate_str("日本語", 6), "日本語");
    }

    #[test]
    fn test_truncate_str_one_char_short() {
        // String that is one character over
        assert_eq!(truncate_str("hello!", 5), "hell…");
    }

    mod proptest_tests {
        use super::*;
        use proptest::prelude::*;
        use unicode_width::UnicodeWidthStr;

        proptest! {
            /// truncate_str should never panic for any input
            #[test]
            fn truncate_str_never_panics(s in ".*", max_width in 0usize..1000) {
                let _ = truncate_str(&s, max_width);
            }

            /// Output width should always be <= max_width
            #[test]
            fn truncate_str_respects_width(s in "\\PC*", max_width in 0usize..100) {
                let result = truncate_str(&s, max_width);
                prop_assert!(
                    result.width() <= max_width,
                    "Result '{}' (width {}) exceeds max_width {}",
                    result,
                    result.width(),
                    max_width
                );
            }

            /// When input fits, output should be unchanged (except possible clone)
            #[test]
            fn truncate_str_preserves_short_strings(s in "\\PC{0,10}", max_width in 20usize..100) {
                let result = truncate_str(&s, max_width);
                // If input width <= max_width, output should equal input
                if s.width() <= max_width {
                    prop_assert_eq!(&result, &s);
                }
            }

            /// Empty string always returns empty
            #[test]
            fn truncate_str_empty_is_empty(max_width in 0usize..100) {
                let result = truncate_str("", max_width);
                prop_assert_eq!(result, "");
            }

            /// Zero max_width always returns empty
            #[test]
            fn truncate_str_zero_width_is_empty(s in ".*") {
                let result = truncate_str(&s, 0);
                prop_assert_eq!(result, "");
            }
        }
    }
}
