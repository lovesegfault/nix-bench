//! Centralized test fixtures and helpers for nix-bench tests.
//!
//! This module provides shared test utilities to avoid duplication across test modules.

/// Test fixtures for agent configuration
#[cfg(all(test, feature = "agent"))]
pub mod agent_fixtures {
    use crate::agent::config::Config;

    /// Create a test Config with sensible defaults
    pub fn test_config() -> Config {
        Config {
            run_id: "test-run-123".to_string(),
            bucket: "test-bucket".to_string(),
            region: "us-east-1".to_string(),
            attr: "small-shallow".to_string(),
            runs: 5,
            instance_type: "c6i.xlarge".to_string(),
            system: "x86_64-linux".to_string(),
            flake_ref: "github:test/repo".to_string(),
            build_timeout: 3600,
            max_failures: 3,
            broadcast_capacity: 1024,
            grpc_port: 50051,
            require_tls: false,
            ca_cert_pem: None,
            agent_cert_pem: None,
            agent_key_pem: None,
        }
    }

    /// Create a test Config with TLS enabled
    pub fn test_config_with_tls() -> Config {
        Config {
            run_id: "test-run-123".to_string(),
            bucket: "test-bucket".to_string(),
            region: "us-east-1".to_string(),
            attr: "small-shallow".to_string(),
            runs: 5,
            instance_type: "c6i.xlarge".to_string(),
            system: "x86_64-linux".to_string(),
            flake_ref: "github:test/repo".to_string(),
            build_timeout: 3600,
            max_failures: 3,
            broadcast_capacity: 1024,
            grpc_port: 50051,
            require_tls: true,
            ca_cert_pem: Some("-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----".to_string()),
            agent_cert_pem: Some("-----BEGIN CERTIFICATE-----\nAGENT\n-----END CERTIFICATE-----".to_string()),
            agent_key_pem: Some("-----BEGIN PRIVATE KEY-----\nKEY\n-----END PRIVATE KEY-----".to_string()),
        }
    }
}

/// Test fixtures for coordinator configuration
#[cfg(all(test, feature = "coordinator"))]
pub mod coordinator_fixtures {
    use crate::config::{RunConfig, DEFAULT_BUILD_TIMEOUT, DEFAULT_FLAKE_REF, DEFAULT_MAX_FAILURES};

    /// Create a minimal valid RunConfig for testing
    pub fn test_run_config() -> RunConfig {
        RunConfig {
            instance_types: vec!["c6i.xlarge".to_string()],
            attr: "small-shallow".to_string(),
            runs: 3,
            region: "us-east-1".to_string(),
            output: None,
            keep: false,
            timeout: 0,
            no_tui: true,
            agent_x86_64: None,
            agent_aarch64: None,
            subnet_id: None,
            security_group_id: None,
            instance_profile: None,
            dry_run: false,
            flake_ref: DEFAULT_FLAKE_REF.to_string(),
            build_timeout: DEFAULT_BUILD_TIMEOUT,
            max_failures: DEFAULT_MAX_FAILURES,
            log_capture: None,
        }
    }

    /// Create a RunConfig with multiple instance types
    pub fn test_run_config_multi_instance() -> RunConfig {
        RunConfig {
            instance_types: vec![
                "c6i.xlarge".to_string(),
                "c6g.xlarge".to_string(),
                "c7i.xlarge".to_string(),
            ],
            attr: "small-shallow".to_string(),
            runs: 5,
            region: "us-east-1".to_string(),
            output: Some("/tmp/results.json".to_string()),
            keep: false,
            timeout: 3600,
            no_tui: false,
            agent_x86_64: None,
            agent_aarch64: None,
            subnet_id: None,
            security_group_id: None,
            instance_profile: None,
            dry_run: false,
            flake_ref: DEFAULT_FLAKE_REF.to_string(),
            build_timeout: DEFAULT_BUILD_TIMEOUT,
            max_failures: DEFAULT_MAX_FAILURES,
            log_capture: None,
        }
    }
}

/// Test fixtures for TUI components
#[cfg(all(test, feature = "coordinator"))]
pub mod tui_fixtures {
    use crate::orchestrator::{InstanceState, InstanceStatus};
    use crate::tui::{App, LogBuffer};

    /// Create a minimal App for testing
    pub fn test_app() -> App {
        App::new_loading(&["test-instance".to_string()], 3) // 3 total runs per instance
    }

    /// Create an App with pre-populated instances
    pub fn test_app_with_instances() -> App {
        let instance_types = vec!["c6i.xlarge".to_string(), "c6g.xlarge".to_string()];
        let mut app = App::new_loading(&instance_types, 5);

        // Update the instances with IDs and status directly
        if let Some(state) = app.instances.data.get_mut("c6i.xlarge") {
            state.instance_id = "i-123456".to_string();
            state.status = InstanceStatus::Running;
            state.public_ip = Some("10.0.0.1".to_string());
        }
        if let Some(state) = app.instances.data.get_mut("c6g.xlarge") {
            state.instance_id = "i-789012".to_string();
            state.status = InstanceStatus::Running;
            state.public_ip = Some("10.0.0.2".to_string());
        }

        app
    }

    /// Create test InstanceState
    pub fn test_instance_state(instance_type: &str, instance_id: &str) -> InstanceState {
        InstanceState {
            instance_id: instance_id.to_string(),
            instance_type: instance_type.to_string(),
            system: "x86_64-linux".to_string(),
            status: InstanceStatus::Running,
            run_progress: 0,
            total_runs: 5,
            durations: Vec::new(),
            public_ip: Some("10.0.0.1".to_string()),
            console_output: LogBuffer::default(),
        }
    }
}

/// Helper utilities for async tests
#[cfg(test)]
pub mod test_utils {
    use tokio::net::TcpListener;

    /// Find an available port for testing
    pub async fn find_available_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    }

    /// Wait for a TCP port to be available
    pub async fn wait_for_port(port: u16, timeout_ms: u64) -> bool {
        use std::time::Duration;
        use tokio::time::{sleep, timeout};

        let deadline = Duration::from_millis(timeout_ms);
        let result = timeout(deadline, async {
            loop {
                match tokio::net::TcpStream::connect(format!("127.0.0.1:{}", port)).await {
                    Ok(_) => return true,
                    Err(_) => sleep(Duration::from_millis(50)).await,
                }
            }
        })
        .await;

        result.unwrap_or(false)
    }
}
