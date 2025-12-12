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
use tokio::sync::mpsc;

pub use app::{App, InitPhase};

/// Message sent to TUI to update state
#[derive(Debug, Clone)]
pub enum TuiMessage {
    /// Update init phase
    Phase(InitPhase),
    /// Set run ID and bucket name
    RunInfo { run_id: String, bucket_name: String },
    /// Update instance state
    InstanceUpdate {
        instance_type: String,
        instance_id: String,
        status: crate::orchestrator::InstanceStatus,
        public_ip: Option<String>,
    },
    /// Update console output for an instance
    ConsoleOutput {
        instance_type: String,
        output: String,
    },
}

/// Run the TUI dashboard (legacy, for after-init usage)
pub async fn run_tui(
    instances: &mut HashMap<String, InstanceState>,
    cloudwatch: &CloudWatchClient,
    config: &RunConfig,
) -> Result<()> {
    let (tx, rx) = mpsc::channel(100);
    run_tui_with_channel(instances, cloudwatch, config, rx).await
}

/// Run the TUI dashboard with a channel for receiving updates
pub async fn run_tui_with_channel(
    instances: &mut HashMap<String, InstanceState>,
    cloudwatch: &CloudWatchClient,
    config: &RunConfig,
    mut rx: mpsc::Receiver<TuiMessage>,
) -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app state
    let mut app = if instances.is_empty() {
        App::new_loading(&config.instance_types, config.runs)
    } else {
        App::new(instances.clone(), config.runs)
    };

    // Run the app with channel
    let result = app.run_with_channel(&mut terminal, cloudwatch, config, &mut rx).await;

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

/// Create a TUI message sender for the orchestrator
pub fn create_channel() -> (mpsc::Sender<TuiMessage>, mpsc::Receiver<TuiMessage>) {
    mpsc::channel(100)
}
