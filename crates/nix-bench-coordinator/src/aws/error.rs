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
///
/// This uses the `ProvideErrorMetadata` trait's `.code()` method to extract
/// the error code, avoiding string matching on Debug format.
///
/// # Example
/// ```ignore
/// use aws_sdk_ec2::error::SdkError;
/// use nix_bench::aws::error::classify_aws_error;
///
/// match result {
///     Err(sdk_error) => {
///         let aws_error = classify_aws_error(sdk_error.code(), sdk_error.message());
///         if aws_error.is_not_found() {
///             // Resource already gone, safe to continue
///         }
///     }
///     Ok(_) => {}
/// }
/// ```
pub fn classify_aws_error(code: Option<&str>, message: Option<&str>) -> AwsError {
    let message = message.unwrap_or("Unknown error").to_string();

    match code {
        // Not Found errors
        Some(c) if NOT_FOUND_CODES.contains(&c) => AwsError::NotFound {
            resource_type: "resource",
            resource_id: message.clone(),
        },

        // Already exists errors
        Some(c) if ALREADY_EXISTS_CODES.contains(&c) => AwsError::AlreadyExists,

        // Throttling errors
        Some(c) if THROTTLING_CODES.contains(&c) => AwsError::Throttled,

        // Dependency violations (e.g., SG with attached ENI)
        Some(c) if DEPENDENCY_CODES.contains(&c) => AwsError::DependencyViolation,

        // IAM propagation delay (specific InvalidParameterValue case)
        Some("InvalidParameterValue") if message.contains("iamInstanceProfile") => {
            AwsError::IamPropagationDelay
        }

        // Also check for the alternate IAM error message
        Some(_) if message.contains("Invalid IAM Instance Profile") => {
            AwsError::IamPropagationDelay
        }

        // Fallback to generic SDK error
        _ => AwsError::Sdk {
            code: code.map(|s| s.to_string()),
            message,
        },
    }
}

/// Try to downcast an error chain to known AWS SDK error types and classify them.
///
/// Each arm downcasts to the given `SdkError<OpError>` type and uses
/// `ProvideErrorMetadata::meta()` to extract the error code and message.
macro_rules! try_classify_sdk_error {
    ($error:expr, $( $sdk_error_type:ty ),+ $(,)?) => {
        for cause in $error.chain() {
            $(
                if let Some(sdk_err) = cause.downcast_ref::<$sdk_error_type>() {
                    let meta = aws_sdk_ec2::error::ProvideErrorMetadata::meta(sdk_err);
                    return classify_aws_error(meta.code(), meta.message());
                }
            )+
        }
    };
}

/// Classify an error from an anyhow::Error by extracting the AWS error code.
///
/// First attempts to downcast through the error chain to find an AWS SDK error
/// that implements `ProvideErrorMetadata`, extracting `.code()` and `.message()`
/// directly. Falls back to string matching on the Debug representation if no
/// typed error is found.
pub fn classify_anyhow_error(error: &anyhow::Error) -> AwsError {
    try_classify_sdk_error!(
        error,
        // EC2 operations
        aws_sdk_ec2::error::SdkError<aws_sdk_ec2::operation::run_instances::RunInstancesError>,
        aws_sdk_ec2::error::SdkError<
            aws_sdk_ec2::operation::describe_instances::DescribeInstancesError,
        >,
        aws_sdk_ec2::error::SdkError<
            aws_sdk_ec2::operation::terminate_instances::TerminateInstancesError,
        >,
        aws_sdk_ec2::error::SdkError<
            aws_sdk_ec2::operation::create_security_group::CreateSecurityGroupError,
        >,
        aws_sdk_ec2::error::SdkError<
            aws_sdk_ec2::operation::delete_security_group::DeleteSecurityGroupError,
        >,
        // S3 operations
        aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::delete_bucket::DeleteBucketError>,
        aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::put_object::PutObjectError>,
        aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::get_object::GetObjectError>,
        aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::create_bucket::CreateBucketError>,
        // IAM operations
        aws_sdk_iam::error::SdkError<aws_sdk_iam::operation::create_role::CreateRoleError>,
        aws_sdk_iam::error::SdkError<aws_sdk_iam::operation::delete_role::DeleteRoleError>,
    );

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

/// Structured error details for user-friendly display
#[derive(Debug)]
pub struct AwsErrorDetails {
    /// AWS error code if available (e.g., "InsufficientInstanceCapacity")
    pub code: Option<String>,
    /// Human-readable error message
    pub message: String,
    /// Optional suggestion for resolving the error
    pub suggestion: Option<String>,
}

/// Extract detailed error information from an anyhow::Error.
///
/// This parses the error to extract AWS error codes and provides
/// user-friendly suggestions for common issues.
pub fn extract_error_details(error: &anyhow::Error) -> AwsErrorDetails {
    let error_debug = format!("{:?}", error);
    let error_display = error.to_string();

    // Try to extract the error code from the debug representation
    let code = extract_error_code(&error_debug);

    // Get suggestion based on the error code
    let suggestion = code.as_ref().and_then(|c| suggestion_for_code(c));

    AwsErrorDetails {
        code,
        message: error_display,
        suggestion,
    }
}

/// Known AWS error codes that indicate capacity issues
const CAPACITY_CODES: &[&str] = &[
    "InsufficientInstanceCapacity",
    "InsufficientHostCapacity",
    "InsufficientReservedInstanceCapacity",
    "InsufficientCapacity",
];

/// Known AWS error codes that indicate limit issues
const LIMIT_CODES: &[&str] = &[
    "InstanceLimitExceeded",
    "VcpuLimitExceeded",
    "MaxSpotInstanceCountExceeded",
];

/// Known AWS error codes for unsupported configurations
const UNSUPPORTED_CODES: &[&str] = &["Unsupported", "UnsupportedOperation"];

/// All known AWS error code lists for extraction from debug strings
const ALL_KNOWN_CODE_LISTS: &[&[&str]] = &[
    NOT_FOUND_CODES,
    ALREADY_EXISTS_CODES,
    THROTTLING_CODES,
    DEPENDENCY_CODES,
    CAPACITY_CODES,
    LIMIT_CODES,
    UNSUPPORTED_CODES,
];

/// Extract an AWS error code from a debug string representation
fn extract_error_code(debug_str: &str) -> Option<String> {
    // Check all known code lists
    for code_list in ALL_KNOWN_CODE_LISTS {
        for code in *code_list {
            if debug_str.contains(code) {
                return Some((*code).to_string());
            }
        }
    }

    // Check for IAM propagation delay patterns
    if debug_str.contains("InvalidParameterValue") && debug_str.contains("iamInstanceProfile") {
        return Some("InvalidParameterValue".to_string());
    }
    if debug_str.contains("Invalid IAM Instance Profile") {
        return Some("InvalidParameterValue".to_string());
    }

    // Try to extract any code that looks like an AWS error code pattern
    // Common patterns: "SomeErrorCode" or "Some.ErrorCode"
    if let Some(start) = debug_str.find("code: Some(\"") {
        let rest = &debug_str[start + 12..];
        if let Some(end) = rest.find('"') {
            return Some(rest[..end].to_string());
        }
    }

    None
}

/// Get a user-friendly suggestion for a known error code.
fn suggestion_for_code(code: &str) -> Option<String> {
    match code {
        // Capacity issues
        "InsufficientInstanceCapacity" | "InsufficientHostCapacity" | "InsufficientCapacity" => {
            Some("Try a different availability zone or instance type.".to_string())
        }
        "InsufficientReservedInstanceCapacity" => Some(
            "Your reserved instance capacity is exhausted. Try on-demand instances.".to_string(),
        ),

        // Limit issues
        "InstanceLimitExceeded" | "VcpuLimitExceeded" => {
            Some("Request a service limit increase via AWS Service Quotas console.".to_string())
        }
        "MaxSpotInstanceCountExceeded" => {
            Some("Reduce spot instance count or request a limit increase.".to_string())
        }

        // Unsupported
        "Unsupported" | "UnsupportedOperation" => {
            Some("This instance type may not be available in this region/AZ.".to_string())
        }

        // Throttling
        "Throttling" | "ThrottlingException" | "RequestLimitExceeded" => {
            Some("AWS API rate limit hit. The operation will be retried automatically.".to_string())
        }

        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── classify_aws_error ──────────────────────────────────────────

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
    }

    #[test]
    fn iam_propagation_alternate_message() {
        let err = classify_aws_error(Some("SomeCode"), Some("Invalid IAM Instance Profile name"));
        assert!(matches!(err, AwsError::IamPropagationDelay));
    }

    #[test]
    fn unknown_code_falls_through_to_sdk() {
        let err = classify_aws_error(Some("SomeNewError"), Some("details"));
        assert!(matches!(err, AwsError::Sdk { .. }));
    }

    #[test]
    fn no_code_falls_through_to_sdk() {
        let err = classify_aws_error(None, Some("something failed"));
        assert!(matches!(err, AwsError::Sdk { code: None, .. }));
    }

    // ── extract_error_code ──────────────────────────────────────────

    #[test]
    fn extract_known_codes_from_debug_string() {
        for code_list in ALL_KNOWN_CODE_LISTS {
            for code in *code_list {
                let debug_str = format!("SdkError {{ code: Some(\"{code}\"), message: \"fail\" }}");
                let extracted = extract_error_code(&debug_str);
                assert!(
                    extracted.is_some(),
                    "Failed to extract any code from string containing: {code}"
                );
            }
        }
    }

    #[test]
    fn extract_code_from_code_field() {
        let debug_str = r#"SdkError { code: Some("SomeRandomCode"), message: "fail" }"#;
        let extracted = extract_error_code(debug_str);
        assert_eq!(extracted.as_deref(), Some("SomeRandomCode"));
    }

    #[test]
    fn extract_none_from_unrelated_string() {
        let extracted = extract_error_code("connection refused");
        assert!(extracted.is_none());
    }

    // ── suggestion_for_code ─────────────────────────────────────────

    #[test]
    fn suggestions_exist_for_capacity_codes() {
        for code in CAPACITY_CODES {
            assert!(
                suggestion_for_code(code).is_some(),
                "No suggestion for capacity code: {code}"
            );
        }
    }

    #[test]
    fn suggestions_exist_for_limit_codes() {
        for code in LIMIT_CODES {
            assert!(
                suggestion_for_code(code).is_some(),
                "No suggestion for limit code: {code}"
            );
        }
    }

    #[test]
    fn no_suggestion_for_unknown_code() {
        assert!(suggestion_for_code("SomeUnknownCode").is_none());
    }

    // ── AwsError methods ────────────────────────────────────────────

    #[test]
    fn is_not_found_only_for_not_found() {
        assert!(
            AwsError::NotFound {
                resource_type: "test",
                resource_id: "id".to_string()
            }
            .is_not_found()
        );
        assert!(!AwsError::Throttled.is_not_found());
    }

    #[test]
    fn is_retryable_for_expected_variants() {
        assert!(AwsError::IamPropagationDelay.is_retryable());
        assert!(AwsError::Throttled.is_retryable());
        assert!(AwsError::DependencyViolation.is_retryable());
        assert!(!AwsError::AlreadyExists.is_retryable());
        assert!(
            !AwsError::NotFound {
                resource_type: "test",
                resource_id: "id".to_string()
            }
            .is_retryable()
        );
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// extract_error_code never panics on arbitrary input
        #[test]
        fn extract_error_code_never_panics(input in ".*") {
            let _ = extract_error_code(&input);
        }

        /// classify_aws_error never panics on arbitrary input
        #[test]
        fn classify_aws_error_never_panics(
            code in proptest::option::of("[A-Za-z.]{0,50}"),
            message in proptest::option::of(".{0,200}"),
        ) {
            let _ = classify_aws_error(code.as_deref(), message.as_deref());
        }

        /// Known codes are always correctly extracted from debug strings
        #[test]
        fn known_codes_always_found(
            prefix in ".{0,100}",
            suffix in ".{0,100}",
            code_idx in 0..NOT_FOUND_CODES.len(),
        ) {
            let code = NOT_FOUND_CODES[code_idx];
            let debug_str = format!("{}{}{}", prefix, code, suffix);
            let extracted = extract_error_code(&debug_str);
            prop_assert!(extracted.is_some(), "Failed to extract known code: {}", code);
        }

        /// suggestion_for_code never panics
        #[test]
        fn suggestion_never_panics(code in ".{0,50}") {
            let _ = suggestion_for_code(&code);
        }
    }
}
