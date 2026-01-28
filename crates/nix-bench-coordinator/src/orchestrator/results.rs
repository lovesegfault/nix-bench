//! Results writing and summary display
//!
//! This module handles writing benchmark results to JSON files and
//! printing summary tables to stdout.

use std::collections::HashMap;

use anyhow::Result;
use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, ContentArrangement, Table};
use tracing::info;

use super::types::{InstanceState, InstanceStatus};
use crate::config::RunConfig;

/// Write results to output file
pub async fn write_results(
    config: &RunConfig,
    run_id: &str,
    instances: &HashMap<String, InstanceState>,
) -> Result<()> {
    if let Some(output_path) = &config.output {
        let written_at = chrono::Utc::now();

        let all_complete = instances
            .values()
            .all(|s| s.status == InstanceStatus::Complete);

        let results: HashMap<String, serde_json::Value> = instances
            .iter()
            .map(|(k, v)| {
                let avg = if !v.durations().is_empty() {
                    v.durations().iter().sum::<f64>() / v.durations().len() as f64
                } else {
                    0.0
                };
                let min = v
                    .durations()
                    .iter()
                    .cloned()
                    .filter(|x| x.is_finite())
                    .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                    .unwrap_or(0.0);
                let max = v
                    .durations()
                    .iter()
                    .cloned()
                    .filter(|x| x.is_finite())
                    .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                    .unwrap_or(0.0);

                (
                    k.clone(),
                    serde_json::json!({
                        "instance_id": v.instance_id,
                        "system": v.system,
                        "status": format!("{:?}", v.status),
                        "runs_completed": v.run_progress,
                        "runs_total": v.total_runs,
                        "durations_seconds": v.durations(),
                        "stats": {
                            "avg_seconds": avg,
                            "min_seconds": min,
                            "max_seconds": max,
                        }
                    }),
                )
            })
            .collect();

        let output = serde_json::json!({
            "run_id": run_id,
            "region": config.region,
            "attr": config.attr,
            "runs_per_instance": config.runs,
            "instance_types": config.instance_types,
            "written_at": written_at.to_rfc3339(),
            "success": all_complete,
            "results": results,
        });

        std::fs::write(output_path, serde_json::to_string_pretty(&output)?)?;
        info!(path = %output_path, "Results written");
    }

    Ok(())
}

/// Print a summary table of benchmark results to stdout
pub fn print_results_summary(instances: &HashMap<String, InstanceState>) {
    if instances.is_empty() {
        return;
    }

    println!("\n=== Benchmark Results ===\n");

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("Instance Type"),
            Cell::new("Status"),
            Cell::new("Runs"),
            Cell::new("Min (s)"),
            Cell::new("Avg (s)"),
            Cell::new("Max (s)"),
        ]);

    // Sort by average duration (fastest first), with instances that have no results at the end
    let mut sorted: Vec<_> = instances.iter().collect();
    sorted.sort_by(|(k1, s1), (k2, s2)| {
        let avg1 = if s1.durations().is_empty() {
            f64::MAX
        } else {
            s1.durations().iter().sum::<f64>() / s1.durations().len() as f64
        };
        let avg2 = if s2.durations().is_empty() {
            f64::MAX
        } else {
            s2.durations().iter().sum::<f64>() / s2.durations().len() as f64
        };
        avg1.partial_cmp(&avg2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| k1.cmp(k2)) // tie-breaker: instance type name
    });

    for (instance_type, state) in sorted {
        let status = match state.status {
            InstanceStatus::Pending => "Pending",
            InstanceStatus::Launching => "Launching",
            InstanceStatus::Starting => "Starting",
            InstanceStatus::Running => "Running",
            InstanceStatus::Complete => "Complete",
            InstanceStatus::Failed => "Failed",
            InstanceStatus::Terminated => "Terminated",
        };

        let runs = format!("{}/{}", state.run_progress, state.total_runs);

        let (min, avg, max) = if state.durations().is_empty() {
            ("-".to_string(), "-".to_string(), "-".to_string())
        } else {
            let min_val = state
                .durations()
                .iter()
                .cloned()
                .filter(|x| x.is_finite())
                .fold(f64::MAX, f64::min);
            let avg_val = state.durations().iter().sum::<f64>() / state.durations().len() as f64;
            let max_val = state
                .durations()
                .iter()
                .cloned()
                .filter(|x| x.is_finite())
                .fold(f64::MIN, f64::max);
            (
                format!("{:.1}", min_val),
                format!("{:.1}", avg_val),
                format!("{:.1}", max_val),
            )
        };

        table.add_row(vec![
            Cell::new(instance_type),
            Cell::new(status),
            Cell::new(&runs),
            Cell::new(&min),
            Cell::new(&avg),
            Cell::new(&max),
        ]);
    }

    println!("{table}");
}
