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
}
