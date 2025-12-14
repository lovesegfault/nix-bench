//! Duration statistics utility
//!
//! Provides `DurationStats` for computing min/avg/max statistics from
//! a collection of duration measurements.

/// Statistics for a collection of duration measurements
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DurationStats {
    /// Minimum duration in seconds
    pub min: f64,
    /// Maximum duration in seconds
    pub max: f64,
    /// Average duration in seconds
    pub avg: f64,
    /// Number of valid measurements
    pub count: usize,
}

impl DurationStats {
    /// Compute statistics from a slice of durations (in seconds).
    ///
    /// Filters out non-finite values (NaN, infinity) before computing.
    ///
    /// # Example
    /// ```
    /// use nix_bench_common::stats::DurationStats;
    ///
    /// let durations = vec![1.0, 2.0, 3.0, 4.0, 5.0];
    /// let stats = DurationStats::from_durations(&durations);
    /// assert_eq!(stats.min, 1.0);
    /// assert_eq!(stats.max, 5.0);
    /// assert_eq!(stats.avg, 3.0);
    /// assert_eq!(stats.count, 5);
    /// ```
    pub fn from_durations(durations: &[f64]) -> Self {
        let valid: Vec<f64> = durations.iter().copied().filter(|x| x.is_finite()).collect();

        if valid.is_empty() {
            return Self::default();
        }

        let count = valid.len();
        let sum: f64 = valid.iter().sum();
        let min = valid
            .iter()
            .copied()
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(0.0);
        let max = valid
            .iter()
            .copied()
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(0.0);

        Self {
            min,
            max,
            avg: sum / count as f64,
            count,
        }
    }

    /// Check if no valid durations were provided
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Format as "min/avg/max" string (e.g., "1.2/3.4/5.6s")
    #[cfg(test)]
    pub fn format_short(&self) -> String {
        if self.is_empty() {
            "-".to_string()
        } else {
            format!("{:.1}/{:.1}/{:.1}s", self.min, self.avg, self.max)
        }
    }

    /// Format with labels (e.g., "Avg: 3.4s  Min: 1.2s  Max: 5.6s")
    #[cfg(test)]
    pub fn format_labeled(&self) -> String {
        if self.is_empty() {
            "-".to_string()
        } else {
            format!(
                "Avg: {:.1}s  Min: {:.1}s  Max: {:.1}s",
                self.avg, self.min, self.max
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_durations() {
        let stats = DurationStats::from_durations(&[]);
        assert!(stats.is_empty());
        assert_eq!(stats.count, 0);
        assert_eq!(stats.format_short(), "-");
        assert_eq!(stats.format_labeled(), "-");
    }

    #[test]
    fn test_single_duration() {
        let stats = DurationStats::from_durations(&[5.0]);
        assert_eq!(stats.min, 5.0);
        assert_eq!(stats.max, 5.0);
        assert_eq!(stats.avg, 5.0);
        assert_eq!(stats.count, 1);
    }

    #[test]
    fn test_multiple_durations() {
        let stats = DurationStats::from_durations(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(stats.min, 1.0);
        assert_eq!(stats.max, 5.0);
        assert_eq!(stats.avg, 3.0);
        assert_eq!(stats.count, 5);
    }

    #[test]
    fn test_filters_nan_infinity() {
        let stats = DurationStats::from_durations(&[1.0, f64::NAN, 3.0, f64::INFINITY, 5.0]);
        assert_eq!(stats.count, 3);
        assert_eq!(stats.min, 1.0);
        assert_eq!(stats.max, 5.0);
        assert_eq!(stats.avg, 3.0);
    }

    #[test]
    fn test_all_nan() {
        let stats = DurationStats::from_durations(&[f64::NAN, f64::NAN]);
        assert!(stats.is_empty());
    }

    #[test]
    fn test_format_short() {
        let stats = DurationStats::from_durations(&[1.23, 4.56, 7.89]);
        assert_eq!(stats.format_short(), "1.2/4.6/7.9s");
    }

    #[test]
    fn test_format_labeled() {
        let stats = DurationStats::from_durations(&[1.0, 2.0, 3.0]);
        assert_eq!(stats.format_labeled(), "Avg: 2.0s  Min: 1.0s  Max: 3.0s");
    }

    // Property-based tests
    use proptest::prelude::*;

    proptest! {
        /// DurationStats should never panic regardless of input
        #[test]
        fn duration_stats_never_panics(durations in prop::collection::vec(-1e15..1e15f64, 0..100)) {
            let _ = DurationStats::from_durations(&durations);
        }

        /// avg should always be between min and max (when non-empty)
        #[test]
        fn avg_between_min_and_max(durations in prop::collection::vec(0.0..1000.0f64, 1..50)) {
            let stats = DurationStats::from_durations(&durations);
            if !stats.is_empty() {
                prop_assert!(stats.avg >= stats.min, "avg {} < min {}", stats.avg, stats.min);
                prop_assert!(stats.avg <= stats.max, "avg {} > max {}", stats.avg, stats.max);
            }
        }

        /// count should match the number of finite values
        #[test]
        fn count_matches_finite_values(durations in prop::collection::vec(prop::num::f64::ANY, 0..50)) {
            let stats = DurationStats::from_durations(&durations);
            let expected_count = durations.iter().filter(|x| x.is_finite()).count();
            prop_assert_eq!(stats.count, expected_count);
        }

        /// format_short should not panic
        #[test]
        fn format_short_never_panics(durations in prop::collection::vec(prop::num::f64::ANY, 0..20)) {
            let stats = DurationStats::from_durations(&durations);
            let _ = stats.format_short();
        }

        /// format_labeled should not panic
        #[test]
        fn format_labeled_never_panics(durations in prop::collection::vec(prop::num::f64::ANY, 0..20)) {
            let stats = DurationStats::from_durations(&durations);
            let _ = stats.format_labeled();
        }
    }
}
