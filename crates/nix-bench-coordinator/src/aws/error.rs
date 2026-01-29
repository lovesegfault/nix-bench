//! AWS error classification and handling
//!
//! Provides typed errors for AWS SDK operations using the `.code()` method
//! instead of string matching on Debug format.

use thiserror::Error;

/// AWS error categories for retry and cleanup logic
#[derive(Debug, Error)]
pub enum AwsError {
    /// Resource was not found (safe to skip in cleanup)
    #[error("Resource not found: {resource_type} '{resource_id}'")]
    NotFound {
        resource_type: &'static str,
        resource_id: String,
    },

    /// Resource already exists (safe to ignore in create operations)
    #[error("Resource already exists")]
    AlreadyExists,

    /// IAM profile not yet visible to EC2 (eventual consistency, retryable)
    #[error("IAM profile not yet visible to EC2 (eventual consistency)")]
    IamPropagationDelay,

    /// Rate limit exceeded (retryable with backoff)
    #[error("Rate limit exceeded")]
    Throttled,

    /// Resource has dependent objects (retryable, e.g., SG with attached ENI)
    #[error("Resource has dependent objects")]
    DependencyViolation,

    /// Invalid EC2 instance type(s)
    #[error("Invalid instance type(s): {}", invalid_types.join(", "))]
    InvalidInstanceType { invalid_types: Vec<String> },

    /// Generic AWS SDK error with code and message
    #[error("AWS error: {message}")]
    Sdk {
        code: Option<String>,
        message: String,
    },
}

impl AwsError {
    /// Check if this is a "not found" error
    pub fn is_not_found(&self) -> bool {
        matches!(self, AwsError::NotFound { .. })
    }

    /// Check if this is a retryable error
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            AwsError::IamPropagationDelay | AwsError::Throttled | AwsError::DependencyViolation
        )
    }

    /// Check if this is an "already exists" error
    pub fn is_already_exists(&self) -> bool {
        matches!(self, AwsError::AlreadyExists)
    }

    /// Get a user-friendly suggestion for resolving this error, if available.
    pub fn suggestion(&self) -> Option<String> {
        match self {
            AwsError::Sdk { code: Some(c), .. } => suggestion_for_code(c),
            _ => None,
        }
    }
}

/// Known AWS error codes for "not found" conditions
const NOT_FOUND_CODES: &[&str] = &[
    "InvalidInstanceID.NotFound",
    "InvalidAllocationID.NotFound",
    "InvalidGroup.NotFound",
    "InvalidPermission.NotFound",
    "NoSuchBucket",
    "NoSuchKey",
    "NoSuchEntity",
];

/// Known AWS error codes for "already exists" conditions
const ALREADY_EXISTS_CODES: &[&str] = &[
    "InvalidPermission.Duplicate",
    "InvalidGroup.Duplicate",
    "EntityAlreadyExists",
    "BucketAlreadyOwnedByYou",
];

/// Known AWS error codes for throttling/rate limiting
const THROTTLING_CODES: &[&str] = &["Throttling", "ThrottlingException", "RequestLimitExceeded"];

/// Known AWS error codes for dependency violations (resource still in use)
const DEPENDENCY_CODES: &[&str] = &["DependencyViolation"];

/// Classify an AWS SDK error using the error code.
pub fn classify_aws_error(code: Option<&str>, message: Option<&str>) -> AwsError {
    let message = message.unwrap_or("Unknown error").to_string();

    match code {
        Some(c) if NOT_FOUND_CODES.contains(&c) => AwsError::NotFound {
            resource_type: "resource",
            resource_id: message.clone(),
        },
        Some(c) if ALREADY_EXISTS_CODES.contains(&c) => AwsError::AlreadyExists,
        Some(c) if THROTTLING_CODES.contains(&c) => AwsError::Throttled,
        Some(c) if DEPENDENCY_CODES.contains(&c) => AwsError::DependencyViolation,
        Some("InvalidParameterValue") if message.contains("iamInstanceProfile") => {
            AwsError::IamPropagationDelay
        }
        Some(_) if message.contains("Invalid IAM Instance Profile") => {
            AwsError::IamPropagationDelay
        }
        _ => AwsError::Sdk {
            code: code.map(|s| s.to_string()),
            message,
        },
    }
}

/// Classify an error from an anyhow::Error by extracting the AWS error code.
///
/// Walks the error chain using `ProvideErrorMetadata` to extract `.code()` and
/// `.message()` from any AWS SDK error. Falls back to string matching on the
/// Debug representation if no typed error is found.
pub fn classify_anyhow_error(error: &anyhow::Error) -> AwsError {
    use aws_sdk_ec2::error::ProvideErrorMetadata;

    // Walk the error chain looking for any type that implements ProvideErrorMetadata.
    // AWS SDK operation errors (e.g., RunInstancesError) implement this trait directly.
    for cause in error.chain() {
        // Try EC2 operation errors
        if let Some(e) =
            cause.downcast_ref::<aws_sdk_ec2::error::SdkError<
                aws_sdk_ec2::operation::run_instances::RunInstancesError,
            >>()
        {
            let meta = ProvideErrorMetadata::meta(e);
            return classify_aws_error(meta.code(), meta.message());
        }
        if let Some(e) = cause.downcast_ref::<aws_sdk_ec2::error::SdkError<
            aws_sdk_ec2::operation::describe_instances::DescribeInstancesError,
        >>() {
            let meta = ProvideErrorMetadata::meta(e);
            return classify_aws_error(meta.code(), meta.message());
        }
        if let Some(e) = cause.downcast_ref::<aws_sdk_ec2::error::SdkError<
            aws_sdk_ec2::operation::terminate_instances::TerminateInstancesError,
        >>() {
            let meta = ProvideErrorMetadata::meta(e);
            return classify_aws_error(meta.code(), meta.message());
        }
        if let Some(e) = cause.downcast_ref::<aws_sdk_ec2::error::SdkError<
            aws_sdk_ec2::operation::create_security_group::CreateSecurityGroupError,
        >>() {
            let meta = ProvideErrorMetadata::meta(e);
            return classify_aws_error(meta.code(), meta.message());
        }
        if let Some(e) = cause.downcast_ref::<aws_sdk_ec2::error::SdkError<
            aws_sdk_ec2::operation::delete_security_group::DeleteSecurityGroupError,
        >>() {
            let meta = ProvideErrorMetadata::meta(e);
            return classify_aws_error(meta.code(), meta.message());
        }
        // Try S3 operation errors
        if let Some(e) = cause.downcast_ref::<aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::delete_bucket::DeleteBucketError>>() {
            let meta = ProvideErrorMetadata::meta(e);
            return classify_aws_error(meta.code(), meta.message());
        }
        if let Some(e) = cause.downcast_ref::<aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::create_bucket::CreateBucketError>>() {
            let meta = ProvideErrorMetadata::meta(e);
            return classify_aws_error(meta.code(), meta.message());
        }
        // Try IAM operation errors
        if let Some(e) = cause.downcast_ref::<aws_sdk_iam::error::SdkError<aws_sdk_iam::operation::create_role::CreateRoleError>>() {
            let meta = ProvideErrorMetadata::meta(e);
            return classify_aws_error(meta.code(), meta.message());
        }
        if let Some(e) = cause.downcast_ref::<aws_sdk_iam::error::SdkError<aws_sdk_iam::operation::delete_role::DeleteRoleError>>() {
            let meta = ProvideErrorMetadata::meta(e);
            return classify_aws_error(meta.code(), meta.message());
        }
    }

    // Fallback: extract error code from debug string representation
    let debug_str = format!("{:?}", error);
    if let Some(code) = extract_error_code(&debug_str) {
        return classify_aws_error(Some(&code), Some(&debug_str));
    }

    AwsError::Sdk {
        code: None,
        message: error.to_string(),
    }
}

/// All known AWS error codes for extraction from debug strings (flat list)
const ALL_KNOWN_CODES: &[&str] = &[
    // Not found
    "InvalidInstanceID.NotFound",
    "InvalidAllocationID.NotFound",
    "InvalidGroup.NotFound",
    "InvalidPermission.NotFound",
    "NoSuchBucket",
    "NoSuchKey",
    "NoSuchEntity",
    // Already exists
    "InvalidPermission.Duplicate",
    "InvalidGroup.Duplicate",
    "EntityAlreadyExists",
    "BucketAlreadyOwnedByYou",
    // Throttling
    "Throttling",
    "ThrottlingException",
    "RequestLimitExceeded",
    // Dependency
    "DependencyViolation",
    // Capacity
    "InsufficientInstanceCapacity",
    "InsufficientHostCapacity",
    "InsufficientReservedInstanceCapacity",
    "InsufficientCapacity",
    // Limits
    "InstanceLimitExceeded",
    "VcpuLimitExceeded",
    "MaxSpotInstanceCountExceeded",
    // Unsupported
    "Unsupported",
    "UnsupportedOperation",
];

/// Extract an AWS error code from a debug string representation
fn extract_error_code(debug_str: &str) -> Option<String> {
    for code in ALL_KNOWN_CODES {
        if debug_str.contains(code) {
            return Some((*code).to_string());
        }
    }

    // Check for IAM propagation delay patterns
    if debug_str.contains("InvalidParameterValue") && debug_str.contains("iamInstanceProfile") {
        return Some("InvalidParameterValue".to_string());
    }
    if debug_str.contains("Invalid IAM Instance Profile") {
        return Some("InvalidParameterValue".to_string());
    }

    // Try to extract any code from `code: Some("...")` pattern
    if let Some(start) = debug_str.find("code: Some(\"") {
        let rest = &debug_str[start + 12..];
        if let Some(end) = rest.find('"') {
            return Some(rest[..end].to_string());
        }
    }

    None
}

/// Error code to user-friendly suggestion mapping
const SUGGESTIONS: &[(&str, &str)] = &[
    (
        "InsufficientInstanceCapacity",
        "Try a different availability zone or instance type.",
    ),
    (
        "InsufficientHostCapacity",
        "Try a different availability zone or instance type.",
    ),
    (
        "InsufficientCapacity",
        "Try a different availability zone or instance type.",
    ),
    (
        "InsufficientReservedInstanceCapacity",
        "Your reserved instance capacity is exhausted. Try on-demand instances.",
    ),
    (
        "InstanceLimitExceeded",
        "Request a service limit increase via AWS Service Quotas console.",
    ),
    (
        "VcpuLimitExceeded",
        "Request a service limit increase via AWS Service Quotas console.",
    ),
    (
        "MaxSpotInstanceCountExceeded",
        "Reduce spot instance count or request a limit increase.",
    ),
    (
        "Unsupported",
        "This instance type may not be available in this region/AZ.",
    ),
    (
        "UnsupportedOperation",
        "This instance type may not be available in this region/AZ.",
    ),
    (
        "Throttling",
        "AWS API rate limit hit. The operation will be retried automatically.",
    ),
    (
        "ThrottlingException",
        "AWS API rate limit hit. The operation will be retried automatically.",
    ),
    (
        "RequestLimitExceeded",
        "AWS API rate limit hit. The operation will be retried automatically.",
    ),
];

/// Get a user-friendly suggestion for a known error code.
fn suggestion_for_code(code: &str) -> Option<String> {
    SUGGESTIONS
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, s)| (*s).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_codes() {
        for code in NOT_FOUND_CODES {
            let err = classify_aws_error(Some(code), Some("some message"));
            assert!(err.is_not_found(), "Expected NotFound for code: {code}");
        }
    }

    #[test]
    fn already_exists_codes() {
        for code in ALREADY_EXISTS_CODES {
            let err = classify_aws_error(Some(code), Some("msg"));
            assert!(
                err.is_already_exists(),
                "Expected AlreadyExists for code: {code}"
            );
        }
    }

    #[test]
    fn throttling_codes() {
        for code in THROTTLING_CODES {
            let err = classify_aws_error(Some(code), Some("msg"));
            assert!(err.is_retryable(), "Expected retryable for code: {code}");
            assert!(matches!(err, AwsError::Throttled));
        }
    }

    #[test]
    fn dependency_violation() {
        let err = classify_aws_error(Some("DependencyViolation"), Some("ENI attached"));
        assert!(err.is_retryable());
        assert!(matches!(err, AwsError::DependencyViolation));
    }

    #[test]
    fn iam_propagation_delay() {
        let err = classify_aws_error(
            Some("InvalidParameterValue"),
            Some("Value for parameter iamInstanceProfile is invalid"),
        );
        assert!(matches!(err, AwsError::IamPropagationDelay));
        assert!(err.is_retryable());

        // Alternate message form
        let err2 = classify_aws_error(Some("SomeCode"), Some("Invalid IAM Instance Profile name"));
        assert!(matches!(err2, AwsError::IamPropagationDelay));
    }

    #[test]
    fn unknown_and_missing_codes() {
        let err = classify_aws_error(Some("SomeNewError"), Some("details"));
        assert!(matches!(err, AwsError::Sdk { .. }));

        let err2 = classify_aws_error(None, Some("something failed"));
        assert!(matches!(err2, AwsError::Sdk { code: None, .. }));
    }

    #[test]
    fn extract_known_codes_from_debug_string() {
        for code in ALL_KNOWN_CODES {
            let debug_str = format!("SdkError {{ code: Some(\"{code}\"), message: \"fail\" }}");
            let extracted = extract_error_code(&debug_str);
            assert!(
                extracted.is_some(),
                "Failed to extract any code from string containing: {code}"
            );
        }
    }

    #[test]
    fn extract_code_from_code_field() {
        let debug_str = r#"SdkError { code: Some("SomeRandomCode"), message: "fail" }"#;
        assert_eq!(
            extract_error_code(debug_str).as_deref(),
            Some("SomeRandomCode")
        );
    }

    #[test]
    fn extract_none_from_unrelated_string() {
        assert!(extract_error_code("connection refused").is_none());
    }

    #[test]
    fn suggestions_for_known_codes() {
        for (code, _) in SUGGESTIONS {
            assert!(
                suggestion_for_code(code).is_some(),
                "No suggestion for code: {code}"
            );
        }
        assert!(suggestion_for_code("SomeUnknownCode").is_none());
    }

    #[test]
    fn aws_error_variant_checks() {
        assert!(
            AwsError::NotFound {
                resource_type: "test",
                resource_id: "id".to_string()
            }
            .is_not_found()
        );
        assert!(!AwsError::Throttled.is_not_found());

        assert!(AwsError::IamPropagationDelay.is_retryable());
        assert!(AwsError::Throttled.is_retryable());
        assert!(AwsError::DependencyViolation.is_retryable());
        assert!(!AwsError::AlreadyExists.is_retryable());
    }
}
