//! Results writing and summary display
//!
//! This module handles writing benchmark results to JSON files and
//! printing summary tables to stdout.

use std::collections::HashMap;

use anyhow::Result;
use comfy_table::{Cell, ContentArrangement, Table, presets::UTF8_FULL_CONDENSED};
use nix_bench_common::DurationStats;
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
                let durations = v.durations();
                let stats = DurationStats::from_durations(&durations);

                (
                    k.clone(),
                    serde_json::json!({
                        "instance_id": v.instance_id,
                        "system": v.system,
                        "status": format!("{:?}", v.status),
                        "runs_completed": v.run_progress,
                        "runs_total": v.total_runs,
                        "durations_seconds": durations,
                        "stats": {
                            "avg_seconds": stats.avg,
                            "min_seconds": stats.min,
                            "max_seconds": stats.max,
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
        let stats1 = DurationStats::from_durations(&s1.durations());
        let stats2 = DurationStats::from_durations(&s2.durations());
        let avg1 = if stats1.is_empty() {
            f64::MAX
        } else {
            stats1.avg
        };
        let avg2 = if stats2.is_empty() {
            f64::MAX
        } else {
            stats2.avg
        };
        avg1.partial_cmp(&avg2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| k1.cmp(k2)) // tie-breaker: instance type name
    });

    for (instance_type, state) in sorted {
        let status = state.status.as_str();
        let runs = format!("{}/{}", state.run_progress, state.total_runs);

        let durations = state.durations();
        let stats = DurationStats::from_durations(&durations);
        let (min, avg, max) = if stats.is_empty() {
            ("-".to_string(), "-".to_string(), "-".to_string())
        } else {
            (
                format!("{:.1}", stats.min),
                format!("{:.1}", stats.avg),
                format!("{:.1}", stats.max),
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
