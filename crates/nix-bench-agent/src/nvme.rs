//! NVMe instance store detection and RAID setup

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tracing::{debug, info};

/// Detect NVMe instance store devices (excluding root EBS volume)
///
/// Returns paths to NVMe devices that are instance store (not EBS).
/// These are typically named nvme1n1, nvme2n1, etc. (nvme0n1 is usually root).
pub fn detect_instance_store() -> Result<Option<Vec<PathBuf>>> {
    let mut devices = Vec::new();

    // Read /sys/block to find NVMe devices
    let entries = fs::read_dir("/sys/block").context("Failed to read /sys/block")?;

    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip non-NVMe devices
        if !name_str.starts_with("nvme") {
            continue;
        }

        // Skip partitions (they have 'p' followed by number)
        if name_str.contains('p') {
            continue;
        }

        // Check if this is an instance store by looking at the model
        let model_path = entry.path().join("device/model");
        if let Ok(model) = fs::read_to_string(&model_path) {
            let model = model.trim();
            debug!(device = %name_str, model = %model, "Found NVMe device");

            // Instance store devices typically have "Instance Storage" or similar in model
            // EBS volumes have "Amazon Elastic Block Store" in model
            if model.contains("Instance Storage") || model.contains("NVMe SSD") {
                let device_path = PathBuf::from(format!("/dev/{}", name_str));
                if device_path.exists() {
                    devices.push(device_path);
                }
            }
        }
    }

    // Sort devices by name for consistent ordering
    devices.sort();

    if devices.is_empty() {
        Ok(None)
    } else {
        Ok(Some(devices))
    }
}

/// Setup RAID0 across multiple NVMe devices and mount for Nix
pub fn setup_raid_and_mount(devices: &[PathBuf]) -> Result<()> {
    let mount_point = PathBuf::from("/mnt/nvme");

    // Create mount point
    fs::create_dir_all(&mount_point).context("Failed to create /mnt/nvme")?;

    let device_to_mount = if devices.len() > 1 {
        // Create RAID0 across all devices
        info!(count = devices.len(), "Creating RAID0 array");

        let device_args: Vec<&str> = devices
            .iter()
            .map(|p| p.to_str().context("Device path contains invalid UTF-8"))
            .collect::<Result<Vec<_>>>()?;

        let status = Command::new("mdadm")
            .args([
                "--create",
                "/dev/md0",
                "--level=0",
                "--raid-devices",
                &devices.len().to_string(),
            ])
            .args(&device_args)
            .arg("--force")
            .status()
            .context("Failed to run mdadm")?;

        if !status.success() {
            anyhow::bail!("mdadm failed with status: {}", status);
        }

        PathBuf::from("/dev/md0")
    } else {
        devices[0].clone()
    };

    // Format with ext4
    info!(device = %device_to_mount.display(), "Formatting with ext4");
    let device_str = device_to_mount
        .to_str()
        .context("Device path contains invalid UTF-8")?;
    let status = Command::new("mkfs.ext4")
        .args(["-F", device_str])
        .status()
        .context("Failed to run mkfs.ext4")?;

    if !status.success() {
        anyhow::bail!("mkfs.ext4 failed with status: {}", status);
    }

    // Mount the device
    info!(device = %device_to_mount.display(), mount = %mount_point.display(), "Mounting");
    let mount_str = mount_point
        .to_str()
        .context("Mount point contains invalid UTF-8")?;
    let status = Command::new("mount")
        .args([device_str, mount_str])
        .status()
        .context("Failed to mount device")?;

    if !status.success() {
        anyhow::bail!("mount failed with status: {}", status);
    }

    // Create directories on NVMe
    for dir in &["nix", "tmp", "var"] {
        fs::create_dir_all(mount_point.join(dir))
            .with_context(|| format!("Failed to create {}", dir))?;
    }

    // Bind mount /nix, /tmp, /var to NVMe
    for (source, target) in [
        ("nix", "/nix"),
        ("tmp", "/tmp"),
        ("var", "/var"),
    ] {
        let source_path = mount_point.join(source);

        // Ensure target exists
        fs::create_dir_all(target)?;

        let source_str = source_path
            .to_str()
            .with_context(|| format!("Source path {} contains invalid UTF-8", source))?;

        info!(source = %source_path.display(), target, "Bind mounting");
        let status = Command::new("mount")
            .args(["--bind", source_str, target])
            .status()
            .with_context(|| format!("Failed to bind mount {} to {}", source, target))?;

        if !status.success() {
            anyhow::bail!("bind mount {} to {} failed", source, target);
        }
    }

    Ok(())
}
