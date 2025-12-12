//! TUI dashboard for benchmark monitoring

mod app;
mod events;
mod ui;
pub mod widgets;

use crate::aws::CloudWatchClient;
use crate::config::RunConfig;
use crate::orchestrator::InstanceState;
use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;
use std::collections::HashMap;
use std::io;

pub use app::App;

/// Run the TUI dashboard
pub async fn run_tui(
    instances: &mut HashMap<String, InstanceState>,
    cloudwatch: &CloudWatchClient,
    config: &RunConfig,
) -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app state
    let mut app = App::new(instances.clone(), config.runs);

    // Run the app
    let result = app.run(&mut terminal, cloudwatch, config).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    // Update instance states from app
    for (instance_type, state) in &app.instances {
        if let Some(orig) = instances.get_mut(instance_type) {
            *orig = state.clone();
        }
    }

    result
}
