//! TUI event loop implementations

use super::{App, InitPhase};
use crate::aws::GrpcInstanceStatus;
use crate::config::RunConfig;
use crate::tui::ui;
use anyhow::Result;
use crossterm::event::{Event, KeyEventKind, MouseButton, MouseEventKind};
use futures::StreamExt;
use ratatui::prelude::*;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use super::CTRLC_HANDLER_INSTALLED;

impl App {
    /// Main event loop
    pub async fn run<B: Backend<Error: Send + Sync + 'static>>(
        &mut self,
        terminal: &mut Terminal<B>,
        _config: &RunConfig,
    ) -> Result<()> {
        let (_tx, mut rx) = tokio::sync::mpsc::channel(1);
        let cancel = CancellationToken::new();
        self.run_with_channel(terminal, &mut rx, cancel).await
    }

    /// Main event loop with channel for receiving updates
    pub async fn run_with_channel<B: Backend<Error: Send + Sync + 'static>>(
        &mut self,
        terminal: &mut Terminal<B>,
        rx: &mut tokio::sync::mpsc::Receiver<crate::tui::TuiMessage>,
        cancel: CancellationToken,
    ) -> Result<()> {
        // Set up Ctrl+C handler (only once per process)
        if !CTRLC_HANDLER_INSTALLED.swap(true, Ordering::SeqCst) {
            let cancel_clone = cancel.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    cancel_clone.cancel();
                }
            });
        }

        let mut event_stream = crossterm::event::EventStream::new();
        let mut tick_interval = tokio::time::interval(Duration::from_millis(100));
        let mut render_interval = tokio::time::interval(Duration::from_millis(33));
        let mut grpc_poll_interval = tokio::time::interval(Duration::from_millis(500));

        // Channel for receiving gRPC status updates from background polling task
        let (grpc_tx, mut grpc_rx) =
            tokio::sync::mpsc::channel::<HashMap<String, GrpcInstanceStatus>>(1);

        loop {
            tokio::select! {
                // Check for cancellation (Ctrl+C)
                _ = cancel.cancelled() => {
                    self.lifecycle.should_quit = true;
                }

                // Receive messages from orchestrator
                Some(msg) = rx.recv() => {
                    self.handle_tui_message(msg);
                }

                // Handle terminal events
                maybe_event = event_stream.next() => {
                    if let Some(Ok(event)) = maybe_event {
                        match event {
                            Event::Key(key) if key.kind == KeyEventKind::Press => {
                                use crate::tui::input::KeyHandler;
                                let _ = KeyHandler::handle(self, key, &cancel);
                            }
                            Event::Mouse(mouse) => {
                                match mouse.kind {
                                    MouseEventKind::Down(MouseButton::Left) => {
                                        self.handle_mouse_click(mouse.column, mouse.row);
                                    }
                                    MouseEventKind::ScrollDown => {
                                        self.handle_mouse_scroll(mouse.column, mouse.row, true);
                                    }
                                    MouseEventKind::ScrollUp => {
                                        self.handle_mouse_scroll(mouse.column, mouse.row, false);
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }
                }

                // Tick for app logic
                _ = tick_interval.tick() => {
                    self.tick_throbbers();

                    // Use engine for completion detection when available
                    let (all_complete, all_captured) = if let Some(engine) = &self.engine {
                        (engine.is_all_complete(), engine.all_results_captured())
                    } else {
                        (self.all_complete(), self.all_results_captured())
                    };

                    if matches!(self.lifecycle.init_phase, InitPhase::Running)
                        && all_complete
                        && all_captured
                    {
                        self.lifecycle.init_phase = InitPhase::Completed;
                        if self.completion_time.is_none() {
                            self.completion_time = Some(std::time::Instant::now());
                        }
                    }

                    if let Some(completed_at) = self.completion_time {
                        if completed_at.elapsed() >= Duration::from_secs(2) {
                            self.lifecycle.should_quit = true;
                        }
                    }
                }

                // Render UI
                _ = render_interval.tick() => {
                    tui_logger::move_events();
                    terminal.draw(|f| ui::render(f, self))?;
                }

                // Receive gRPC status updates from background polling task
                Some(status_map) = grpc_rx.recv() => {
                    if !status_map.is_empty() {
                        if let Some(engine) = &mut self.engine {
                            let events = engine.apply_poll_results(status_map).await;
                            self.handle_run_events(events);
                        }
                    }
                }

                // Trigger background gRPC polling via engine (non-blocking spawn)
                _ = grpc_poll_interval.tick() => {
                    if matches!(self.lifecycle.init_phase, InitPhase::Running) {
                        if let Some(engine) = &self.engine {
                            if let Some(req) = engine.prepare_poll() {
                                let tx = grpc_tx.clone();
                                tokio::spawn(async move {
                                    let results = req.execute().await;
                                    let _ = tx.send(results).await;
                                });
                            }
                        }
                    }
                }
            }

            if self.lifecycle.should_quit {
                break;
            }
        }

        Ok(())
    }

    /// Run the cleanup phase TUI loop
    ///
    /// During cleanup, the TUI remains fully interactive (scrolling, navigation, etc.)
    /// but quit requests are ignored since cleanup is already in progress.
    pub async fn run_cleanup_phase<B: ratatui::backend::Backend<Error: Send + Sync + 'static>>(
        &mut self,
        terminal: &mut Terminal<B>,
        cleanup_handle: tokio::task::JoinHandle<anyhow::Result<()>>,
        mut rx: tokio::sync::mpsc::Receiver<super::super::TuiMessage>,
    ) -> anyhow::Result<()> {
        use super::super::TuiMessage;
        use crate::tui::input::KeyHandler;
        use crossterm::event::{MouseButton, MouseEventKind};
        use tokio::time::{Duration, interval};

        let mut event_stream = crossterm::event::EventStream::new();
        let mut render_interval = interval(Duration::from_millis(33)); // ~30fps for smooth UI
        let mut tick_interval = interval(Duration::from_millis(100)); // Throbber animation
        let mut cleanup_done = false;
        let mut cleanup_result: Option<anyhow::Result<()>> = None;

        // Dummy cancel token - we won't actually cancel during cleanup
        let dummy_cancel = tokio_util::sync::CancellationToken::new();

        tokio::pin!(cleanup_handle);

        loop {
            tokio::select! {
                // Handle terminal events (keyboard and mouse)
                maybe_event = event_stream.next() => {
                    if let Some(Ok(event)) = maybe_event {
                        match event {
                            Event::Key(key) if key.kind == KeyEventKind::Press => {
                                // Handle all keys except quit-related ones trigger quit
                                // KeyHandler will try to show quit confirm, but we clear it immediately
                                let _ = KeyHandler::handle(self, key, &dummy_cancel);
                                // During cleanup, suppress quit confirmation
                                self.ui.show_quit_confirm = false;
                            }
                            Event::Mouse(mouse) => {
                                match mouse.kind {
                                    MouseEventKind::Down(MouseButton::Left) => {
                                        self.handle_mouse_click(mouse.column, mouse.row);
                                    }
                                    MouseEventKind::ScrollDown => {
                                        self.handle_mouse_scroll(mouse.column, mouse.row, true);
                                    }
                                    MouseEventKind::ScrollUp => {
                                        self.handle_mouse_scroll(mouse.column, mouse.row, false);
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }
                }

                // Receive cleanup progress updates
                Some(msg) = rx.recv() => {
                    if let TuiMessage::Phase(phase) = msg {
                        self.lifecycle.init_phase = phase;
                    }
                }

                // Tick for throbber animation
                _ = tick_interval.tick() => {
                    self.tick_throbbers();
                }

                // Render UI
                _ = render_interval.tick() => {
                    tui_logger::move_events();
                    terminal.draw(|f| ui::render(f, self))?;

                    if cleanup_done {
                        self.lifecycle.init_phase = InitPhase::Completed;
                        terminal.draw(|f| ui::render(f, self))?;
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        return cleanup_result.unwrap_or(Ok(()));
                    }
                }

                // Wait for cleanup to complete
                result = &mut cleanup_handle, if !cleanup_done => {
                    cleanup_done = true;
                    cleanup_result = Some(result?);
                }
            }
        }
    }

    /// Process a TUI message, updating app state accordingly.
    fn handle_tui_message(&mut self, msg: crate::tui::TuiMessage) {
        use crate::orchestrator::InstanceStatus;
        use crate::tui::TuiMessage;

        match msg {
            TuiMessage::Phase(phase) => {
                self.lifecycle.init_phase = phase;
            }
            TuiMessage::AccountInfo { account_id } => {
                self.aws_account_id = Some(account_id);
            }
            TuiMessage::InitComplete(engine) => {
                self.engine = Some(*engine);
                self.lifecycle.init_phase = InitPhase::Running;
            }
            TuiMessage::InstanceUpdate {
                instance_type,
                instance_id,
                status,
                public_ip,
                run_progress,
            } => {
                if let Some(state) = self.instances.data.get_mut(&instance_type) {
                    state.instance_id = instance_id;
                    // Only allow transition to Terminated once terminated (terminal state)
                    if state.status != InstanceStatus::Terminated
                        || status == InstanceStatus::Terminated
                    {
                        state.status = status;
                    }
                    state.public_ip = public_ip;
                    if let Some(rp) = run_progress {
                        state.run_progress = rp;
                    }
                }
                // Re-sort instances by average duration (fastest first)
                self.sort_instances_by_duration();
            }
            TuiMessage::ConsoleOutput {
                instance_type,
                output,
            } => {
                if let Some(engine) = &mut self.engine {
                    if let Some(state) = engine.instances_mut().get_mut(&instance_type) {
                        state.console_output.replace(&output);
                    }
                } else if let Some(state) = self.instances.data.get_mut(&instance_type) {
                    state.console_output.replace(&output);
                }
            }
            TuiMessage::ConsoleOutputAppend {
                instance_type,
                line,
            } => {
                if let Some(engine) = &mut self.engine {
                    if let Some(state) = engine.instances_mut().get_mut(&instance_type) {
                        state.console_output.push_line(line);
                    }
                } else if let Some(state) = self.instances.data.get_mut(&instance_type) {
                    state.console_output.push_line(line);
                }
            }
        }
    }

    /// Handle events from the RunEngine, updating the App's view state.
    fn handle_run_events(&mut self, events: Vec<crate::orchestrator::RunEvent>) {
        if !events.is_empty() {
            self.sort_instances_by_duration();
        }
    }
}
