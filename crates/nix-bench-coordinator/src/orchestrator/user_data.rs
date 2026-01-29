//! User data and bootstrap-related functions
//!
//! This module handles EC2 user-data script generation, agent binary discovery,
//! and bootstrap failure detection.

use std::path::Path;

/// Patterns that indicate cloud-init/bootstrap failure
const BOOTSTRAP_FAILURE_PATTERNS: &[&str] = &[
    "unbound variable",
    "Failed to start cloud-final",
    "FAILED] Failed to start",
    "cc_scripts_user.py[WARNING]: Failed to run module scripts-user",
    "nix-bench-agent: command not found",
    "No such file or directory",
];

/// Check if console output indicates a bootstrap failure
pub fn detect_bootstrap_failure(console_output: &str) -> Option<String> {
    for pattern in BOOTSTRAP_FAILURE_PATTERNS {
        if console_output.contains(pattern) {
            return Some(pattern.to_string());
        }
    }
    None
}

/// Validate that a user-data input is safe for shell interpolation.
///
/// Rejects characters that could break double-quoted bash strings
/// or enable injection (`"`, `\`, `` ` ``, `$`, newlines).
fn validate_shell_input(value: &str, field_name: &str) -> Result<(), String> {
    const FORBIDDEN: &[char] = &['"', '\\', '`', '$', '\n', '\r'];
    if let Some(bad) = value.chars().find(|c| FORBIDDEN.contains(c)) {
        return Err(format!(
            "{field_name} contains forbidden character: {bad:?}"
        ));
    }
    if value.is_empty() {
        return Err(format!("{field_name} cannot be empty"));
    }
    Ok(())
}

/// Generate user-data script for an instance.
///
/// The agent now handles all setup (NVMe, Nix installation) internally.
/// This script just downloads and starts the agent with CLI args.
///
/// # Panics
/// Panics if any input contains characters unsafe for shell interpolation.
pub fn generate_user_data(bucket: &str, run_id: &str, instance_type: &str) -> String {
    // Validate inputs before interpolation into shell script
    validate_shell_input(bucket, "bucket").expect("invalid bucket name for user data");
    validate_shell_input(run_id, "run_id").expect("invalid run_id for user data");
    validate_shell_input(instance_type, "instance_type")
        .expect("invalid instance_type for user data");
    format!(
        r#"#!/bin/bash
set -euo pipefail

exec > >(tee /var/log/nix-bench-bootstrap.log) 2>&1

BUCKET="{bucket}"
RUN_ID="{run_id}"
INSTANCE_TYPE="{instance_type}"
ARCH=$(uname -m)

# Download and run agent (agent handles all setup internally)
echo "Fetching agent from S3..."
aws s3 cp "s3://${{BUCKET}}/${{RUN_ID}}/agent-${{ARCH}}" /usr/local/bin/nix-bench-agent
chmod +x /usr/local/bin/nix-bench-agent

echo "Starting nix-bench-agent..."
exec /usr/local/bin/nix-bench-agent \
    --bucket "$BUCKET" \
    --run-id "$RUN_ID" \
    --instance-type "$INSTANCE_TYPE"
"#,
        bucket = bucket,
        run_id = run_id,
        instance_type = instance_type,
    )
}

/// Try to find agent binary in common locations
/// Prefers musl (statically linked) over gnu (dynamically linked)
pub fn find_agent_binary(arch: &str) -> Option<String> {
    // Prefer musl for static linking, fall back to gnu
    let target_triples: &[&str] = match arch {
        "x86_64" => &["x86_64-unknown-linux-musl", "x86_64-unknown-linux-gnu"],
        "aarch64" => &["aarch64-unknown-linux-musl", "aarch64-unknown-linux-gnu"],
        _ => return None,
    };

    for target_triple in target_triples {
        let candidates = [
            // Cross-compiled release build (cargo build --target)
            format!("target/{}/release/nix-bench-agent", target_triple),
            // Relative to crates directory
            format!("../target/{}/release/nix-bench-agent", target_triple),
        ];

        for path in &candidates {
            let p = Path::new(path);
            if p.exists() {
                return Some(p.canonicalize().ok()?.to_string_lossy().to_string());
            }
        }
    }

    // Also check native build (only useful for x86_64 on x86_64 host, but dynamically linked)
    let native_candidates = [
        "target/release/nix-bench-agent",
        "../target/release/nix-bench-agent",
    ];
    for path in &native_candidates {
        let p = Path::new(path);
        if p.exists() {
            return Some(p.canonicalize().ok()?.to_string_lossy().to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_bootstrap_failure_matches_patterns() {
        // Each known failure pattern should be detected
        assert!(detect_bootstrap_failure("line 15: BUCKET: unbound variable").is_some());
        assert!(detect_bootstrap_failure("[FAILED] Failed to start cloud-final.service").is_some());
        assert!(detect_bootstrap_failure("nix-bench-agent: command not found").is_some());
        assert!(detect_bootstrap_failure("No such file or directory").is_some());
        // First pattern wins
        let r = detect_bootstrap_failure("unbound variable\ncommand not found").unwrap();
        assert!(r.contains("unbound variable"));
    }

    #[test]
    fn detect_bootstrap_failure_no_match() {
        assert!(detect_bootstrap_failure("").is_none());
        assert!(detect_bootstrap_failure("Agent started successfully").is_none());
    }

    #[test]
    fn generate_user_data_contains_required_elements() {
        let script = generate_user_data("my-bucket", "run-123", "c6i.xlarge");
        assert!(script.starts_with("#!/bin/bash"));
        assert!(script.contains("set -euo pipefail"));
        assert!(script.contains("BUCKET=\"my-bucket\""));
        assert!(script.contains("RUN_ID=\"run-123\""));
        assert!(script.contains("INSTANCE_TYPE=\"c6i.xlarge\""));
        assert!(script.contains("aws s3 cp"));
        assert!(script.contains("exec /usr/local/bin/nix-bench-agent"));
        assert!(script.contains("${ARCH}"));
    }

    #[test]
    #[should_panic(expected = "invalid bucket name")]
    fn generate_user_data_rejects_shell_injection() {
        generate_user_data("bucket\"; rm -rf /; echo \"", "run-123", "c6i.xlarge");
    }

    #[test]
    #[should_panic(expected = "invalid run_id")]
    fn generate_user_data_rejects_dollar_sign() {
        generate_user_data("bucket", "$(whoami)", "c6i.xlarge");
    }

    #[test]
    fn validate_shell_input_accepts_and_rejects() {
        assert!(validate_shell_input("nix-bench-abc123", "test").is_ok());
        assert!(validate_shell_input("c7i.metal", "test").is_ok());
        assert!(validate_shell_input("", "test").is_err());
    }

    #[test]
    fn find_agent_binary_invalid_arch() {
        assert!(find_agent_binary("arm").is_none());
        assert!(find_agent_binary("").is_none());
    }
}
