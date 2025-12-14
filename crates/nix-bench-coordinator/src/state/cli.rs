//! CLI operations for state management

use super::db::open_db;
use super::queries::get_undeleted_resources;
use anyhow::Result;
use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, ContentArrangement, Table};

/// List all tracked resources
pub async fn list_resources() -> Result<()> {
    let pool = open_db().await?;
    let resources = get_undeleted_resources(&pool).await?;

    if resources.is_empty() {
        println!("No tracked resources");
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("Type"),
            Cell::new("ID"),
            Cell::new("Region"),
            Cell::new("Account"),
            Cell::new("Created"),
        ]);

    for resource in resources {
        table.add_row(vec![
            Cell::new(resource.resource_type.as_str()),
            Cell::new(&resource.resource_id),
            Cell::new(&resource.region),
            Cell::new(&resource.account_id),
            Cell::new(resource.created_at.format("%Y-%m-%d %H:%M:%S").to_string()),
        ]);
    }

    println!("{table}");

    Ok(())
}

/// Remove stale entries from the database
pub async fn prune_database() -> Result<()> {
    let pool = open_db().await?;

    let result = sqlx::query(
        "DELETE FROM resources
         WHERE deleted_at IS NOT NULL
         AND datetime(deleted_at) < datetime('now', '-30 days')",
    )
    .execute(&pool)
    .await?;

    println!("Pruned {} old resource records", result.rows_affected());

    let result = sqlx::query(
        "DELETE FROM runs
         WHERE status = 'completed'
         AND datetime(created_at) < datetime('now', '-30 days')
         AND NOT EXISTS (
             SELECT 1 FROM resources WHERE resources.run_id = runs.run_id
         )",
    )
    .execute(&pool)
    .await?;

    println!("Pruned {} old run records", result.rows_affected());

    Ok(())
}
