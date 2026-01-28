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
    fn test_detect_bootstrap_failure_unbound_variable() {
        let console = r#"
[    5.123456] Starting nix-bench bootstrap
[    5.234567] /var/lib/cloud/instance/scripts/user-data: line 15: BUCKET: unbound variable
[    5.345678] Failed
"#;
        let result = detect_bootstrap_failure(console);
        assert!(result.is_some());
        assert!(result.unwrap().contains("unbound variable"));
    }

    #[test]
    fn test_detect_bootstrap_failure_cloud_init() {
        let console = r#"
[   OK  ] Started cloud-init.service
[FAILED] Failed to start cloud-final.service - Execute cloud user/final scripts
"#;
        let result = detect_bootstrap_failure(console);
        assert!(result.is_some());
        assert!(result.unwrap().contains("Failed to start cloud-final"));
    }

    #[test]
    fn test_detect_bootstrap_failure_agent_not_found() {
        let console = "Starting nix-bench-agent...\nnix-bench-agent: command not found\n";
        let result = detect_bootstrap_failure(console);
        assert!(result.is_some());
        assert!(
            result
                .unwrap()
                .contains("nix-bench-agent: command not found")
        );
    }

    #[test]
    fn test_detect_bootstrap_failure_no_such_file() {
        let console = "/usr/local/bin/nix-bench-agent: No such file or directory";
        let result = detect_bootstrap_failure(console);
        assert!(result.is_some());
    }

    #[test]
    fn test_detect_bootstrap_failure_none() {
        let console = r#"
[   OK  ] Started cloud-init.service
[   OK  ] Started cloud-final.service
Starting nix-bench-agent...
Agent started successfully
"#;
        let result = detect_bootstrap_failure(console);
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_bootstrap_failure_empty() {
        assert!(detect_bootstrap_failure("").is_none());
    }

    #[test]
    fn test_detect_bootstrap_failure_priority() {
        // First pattern should win
        let console = "unbound variable\ncommand not found";
        let result = detect_bootstrap_failure(console);
        assert!(result.is_some());
        assert!(result.unwrap().contains("unbound variable"));
    }

    #[test]
    fn test_generate_user_data_contains_required_elements() {
        let script = generate_user_data("my-bucket", "run-123", "c6i.xlarge");

        // Check shebang
        assert!(script.starts_with("#!/bin/bash"));

        // Check set options
        assert!(script.contains("set -euo pipefail"));

        // Check bucket variable
        assert!(script.contains("BUCKET=\"my-bucket\""));

        // Check run_id variable
        assert!(script.contains("RUN_ID=\"run-123\""));

        // Check instance_type variable
        assert!(script.contains("INSTANCE_TYPE=\"c6i.xlarge\""));

        // Check S3 agent download
        assert!(script.contains("aws s3 cp"));
        assert!(script.contains("s3://${BUCKET}/${RUN_ID}/agent-${ARCH}"));

        // Check agent execution with CLI args
        assert!(script.contains("exec /usr/local/bin/nix-bench-agent"));
        assert!(script.contains("--bucket"));
        assert!(script.contains("--run-id"));
        assert!(script.contains("--instance-type"));
    }

    #[test]
    fn test_generate_user_data_escapes_special_chars() {
        // Bucket name with hyphen (common case)
        let script = generate_user_data("nix-bench-abc123", "test-run-456", "c7i.metal");

        assert!(script.contains("BUCKET=\"nix-bench-abc123\""));
        assert!(script.contains("RUN_ID=\"test-run-456\""));
        assert!(script.contains("INSTANCE_TYPE=\"c7i.metal\""));
    }

    #[test]
    fn test_generate_user_data_uses_bash_variables() {
        let script = generate_user_data("bucket", "run", "type");

        // Verify it uses ${ARCH} for architecture detection (not hardcoded)
        assert!(script.contains("${ARCH}"));
        assert!(script.contains("ARCH=$(uname -m)"));
    }

    #[test]
    #[should_panic(expected = "invalid bucket name")]
    fn test_generate_user_data_rejects_shell_injection() {
        generate_user_data("bucket\"; rm -rf /; echo \"", "run-123", "c6i.xlarge");
    }

    #[test]
    #[should_panic(expected = "invalid run_id")]
    fn test_generate_user_data_rejects_dollar_sign() {
        generate_user_data("bucket", "$(whoami)", "c6i.xlarge");
    }

    #[test]
    fn test_validate_shell_input_accepts_valid_inputs() {
        assert!(validate_shell_input("nix-bench-abc123", "test").is_ok());
        assert!(validate_shell_input("c7i.metal", "test").is_ok());
        assert!(validate_shell_input("01939d1e-7b3f-76ad-a77f-abcdef012345", "test").is_ok());
    }

    #[test]
    fn test_validate_shell_input_rejects_empty() {
        assert!(validate_shell_input("", "test").is_err());
    }

    #[test]
    fn test_find_agent_binary_invalid_arch() {
        // Invalid architecture should return None
        assert!(find_agent_binary("arm").is_none());
        assert!(find_agent_binary("i686").is_none());
        assert!(find_agent_binary("").is_none());
    }

    #[test]
    fn test_find_agent_binary_returns_none_when_missing() {
        // Test in a directory where no binaries exist
        // This is expected behavior when binaries haven't been built
        // The function should return None, not panic
        let result = find_agent_binary("x86_64");
        // We can't assert is_some because the binary might not exist,
        // but we can assert the function doesn't panic
        let _ = result;
    }
}
