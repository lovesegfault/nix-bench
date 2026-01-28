//! Host bootstrap: NVMe setup and Nix installation
//!
//! This module handles all host setup that was previously done in cloud-init user-data.
//! All output is streamed via gRPC for real-time visibility in the TUI.

use crate::command::{CommandConfig, run_command_streaming};
use crate::grpc::LogBroadcaster;
use anyhow::{Context, Result};
use nix_bench_common::timestamp_millis;
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, info};

/// Broadcast a log message to gRPC clients
fn log_message(broadcaster: &Arc<LogBroadcaster>, message: &str) {
    broadcaster.broadcast(timestamp_millis(), message.to_string());
    info!("{}", message);
}

/// Detect NVMe instance store devices (not EBS volumes)
async fn detect_nvme_devices() -> Result<Vec<String>> {
    let mut devices = Vec::new();

    let sys_block = Path::new("/sys/block");
    if !sys_block.exists() {
        return Ok(devices);
    }

    let entries = std::fs::read_dir(sys_block).context("Failed to read /sys/block")?;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Only look at nvme devices, skip partitions
        if !name_str.starts_with("nvme") || name_str.contains('p') {
            continue;
        }

        // Read device model to distinguish instance store from EBS
        let model_path = entry.path().join("device/model");
        if let Ok(model) = std::fs::read_to_string(&model_path) {
            let model = model.trim().replace(' ', "");
            // Instance store devices have "InstanceStorage" or "NVMeSSD" in model
            // EBS volumes have "Amazon Elastic Block Store"
            if model.contains("InstanceStorage") || model.contains("NVMeSSD") {
                devices.push(format!("/dev/{}", name_str));
            }
        }
    }

    devices.sort();
    Ok(devices)
}

/// Setup NVMe instance store for /nix if available
///
/// This detects NVMe instance store devices, creates a RAID0 array if multiple,
/// formats with ext4, and bind-mounts to /nix for fast Nix store operations.
pub(crate) async fn setup_nvme(broadcaster: &Arc<LogBroadcaster>) -> Result<()> {
    log_message(broadcaster, "=== NVMe Instance Store Setup ===");

    let devices = detect_nvme_devices().await?;
    let count = devices.len();

    if count == 0 {
        log_message(
            broadcaster,
            "No NVMe instance store devices found, using default storage",
        );
        return Ok(());
    }

    log_message(
        broadcaster,
        &format!(
            "Found {} NVMe instance store device(s): {:?}",
            count, devices
        ),
    );

    let target_dev = if count > 1 {
        // Create RAID0 array
        log_message(broadcaster, "Creating RAID0 array...");

        // Install mdadm if not present
        if !Path::new("/sbin/mdadm").exists() {
            log_message(broadcaster, "Installing mdadm...");
            run_command_streaming(
                broadcaster,
                "yum",
                &["install", "-y", "mdadm"],
                &CommandConfig::with_timeout_secs(120),
            )
            .await?;
        }

        // Build mdadm command
        let raid_devices_arg = format!("--raid-devices={}", count);
        let mut mdadm_args: Vec<&str> =
            vec!["--create", "/dev/md0", "--level=0", &raid_devices_arg];
        for dev in &devices {
            mdadm_args.push(dev);
        }
        mdadm_args.push("--force");

        run_command_streaming(
            broadcaster,
            "mdadm",
            &mdadm_args,
            &CommandConfig::with_timeout_secs(60),
        )
        .await?;

        "/dev/md0".to_string()
    } else {
        devices[0].clone()
    };

    log_message(
        broadcaster,
        &format!("Formatting {} with ext4...", target_dev),
    );
    run_command_streaming(
        broadcaster,
        "mkfs.ext4",
        &["-F", &target_dev],
        &CommandConfig::with_timeout_secs(120),
    )
    .await?;

    log_message(broadcaster, "Mounting NVMe storage...");

    // Create mount point
    std::fs::create_dir_all("/mnt/nvme").context("Failed to create /mnt/nvme")?;

    run_command_streaming(
        broadcaster,
        "mount",
        &[&target_dev, "/mnt/nvme"],
        &CommandConfig::with_timeout_secs(30),
    )
    .await?;

    // Create directories on NVMe
    std::fs::create_dir_all("/mnt/nvme/nix").context("Failed to create /mnt/nvme/nix")?;
    std::fs::create_dir_all("/mnt/nvme/tmp").context("Failed to create /mnt/nvme/tmp")?;
    std::fs::create_dir_all("/mnt/nvme/var").context("Failed to create /mnt/nvme/var")?;

    // Create /nix and bind mount
    std::fs::create_dir_all("/nix").context("Failed to create /nix")?;

    log_message(broadcaster, "Bind mounting /mnt/nvme/nix to /nix...");
    run_command_streaming(
        broadcaster,
        "mount",
        &["--bind", "/mnt/nvme/nix", "/nix"],
        &CommandConfig::with_timeout_secs(30),
    )
    .await?;

    log_message(broadcaster, "NVMe setup complete");
    Ok(())
}

/// Install Nix using the Determinate Systems installer
pub(crate) async fn install_nix(broadcaster: &Arc<LogBroadcaster>) -> Result<()> {
    log_message(broadcaster, "=== Nix Installation ===");
    log_message(
        broadcaster,
        "Installing upstream Nix via Determinate installer...",
    );

    // Download installer script
    let installer_url = "https://install.determinate.systems/nix";

    // Use curl to download and pipe to sh
    // We need to use sh -c to handle the pipe
    let install_cmd = format!(
        "curl --proto '=https' --tlsv1.2 -sSf -L {} | sh -s -- install --no-confirm --prefer-upstream-nix",
        installer_url
    );

    let success = run_command_streaming(
        broadcaster,
        "sh",
        &["-c", &install_cmd],
        &CommandConfig::for_bootstrap(), // 10 minute timeout
    )
    .await?;

    if !success {
        return Err(anyhow::anyhow!("Nix installation failed"));
    }

    log_message(broadcaster, "Nix installation complete");
    Ok(())
}

/// Source the Nix profile and update PATH
///
/// This modifies the current process environment to include Nix binaries.
pub(crate) fn source_nix_profile() -> Result<()> {
    info!("Sourcing Nix profile...");

    // The Determinate installer puts the profile script at this location
    let profile_script = "/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh";

    if !Path::new(profile_script).exists() {
        return Err(anyhow::anyhow!(
            "Nix profile script not found at {}",
            profile_script
        ));
    }

    // We can't source a bash script directly in Rust, but we can set the PATH
    // The key paths added by the Nix profile are:
    // - /nix/var/nix/profiles/default/bin
    // - ~/.nix-profile/bin (which is /root/.nix-profile/bin for root)

    let nix_paths = [
        "/nix/var/nix/profiles/default/bin",
        "/root/.nix-profile/bin",
    ];

    let current_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", nix_paths.join(":"), current_path);
    // SAFETY: This runs during single-threaded agent bootstrap before any
    // concurrent tasks are spawned, so no other threads can observe the env.
    unsafe { std::env::set_var("PATH", &new_path) };

    // Also set NIX_SSL_CERT_FILE if not set (needed for Nix to fetch from https)
    if std::env::var("NIX_SSL_CERT_FILE").is_err() {
        // Common locations for CA certificates
        let cert_locations = [
            "/etc/ssl/certs/ca-certificates.crt",
            "/etc/pki/tls/certs/ca-bundle.crt",
            "/etc/ssl/ca-bundle.pem",
            "/nix/var/nix/profiles/default/etc/ssl/certs/ca-bundle.crt",
        ];
        for loc in cert_locations {
            if Path::new(loc).exists() {
                // SAFETY: Same as above - single-threaded bootstrap phase.
                unsafe { std::env::set_var("NIX_SSL_CERT_FILE", loc) };
                debug!(path = %loc, "Set NIX_SSL_CERT_FILE");
                break;
            }
        }
    }

    info!(path = %new_path, "Updated PATH with Nix binaries");
    Ok(())
}

/// Run the complete bootstrap sequence
///
/// This sets up NVMe (if available), installs Nix, and sources the profile.
/// All output is streamed via gRPC to the coordinator TUI.
pub async fn run_bootstrap(broadcaster: &Arc<LogBroadcaster>) -> Result<()> {
    log_message(broadcaster, "=== Starting Bootstrap ===");

    // Step 1: Setup NVMe instance store (if available)
    setup_nvme(broadcaster).await?;

    // Step 2: Install Nix
    install_nix(broadcaster).await?;

    // Step 3: Source Nix profile
    source_nix_profile()?;

    log_message(broadcaster, "=== Bootstrap Complete ===");
    Ok(())
}
