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

/// Classify an error from an anyhow::Error by extracting the AWS error code.
///
/// First attempts to downcast through the error chain to find an AWS SDK error
/// that implements `ProvideErrorMetadata`, extracting `.code()` and `.message()`
/// directly. Falls back to string matching on the Debug representation if no
/// typed error is found.
pub fn classify_anyhow_error(error: &anyhow::Error) -> AwsError {
    // Walk the error chain looking for typed AWS SDK errors
    for cause in error.chain() {
        if let Some(sdk_err) = cause.downcast_ref::<aws_sdk_ec2::error::SdkError<aws_sdk_ec2::operation::run_instances::RunInstancesError>>() {
            let meta = aws_sdk_ec2::error::ProvideErrorMetadata::meta(sdk_err);
            return classify_aws_error(meta.code(), meta.message());
        }
        if let Some(sdk_err) = cause.downcast_ref::<aws_sdk_ec2::error::SdkError<aws_sdk_ec2::operation::describe_instances::DescribeInstancesError>>() {
            let meta = aws_sdk_ec2::error::ProvideErrorMetadata::meta(sdk_err);
            return classify_aws_error(meta.code(), meta.message());
        }
        if let Some(sdk_err) = cause.downcast_ref::<aws_sdk_ec2::error::SdkError<aws_sdk_ec2::operation::terminate_instances::TerminateInstancesError>>() {
            let meta = aws_sdk_ec2::error::ProvideErrorMetadata::meta(sdk_err);
            return classify_aws_error(meta.code(), meta.message());
        }
        if let Some(sdk_err) = cause.downcast_ref::<aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::delete_bucket::DeleteBucketError>>() {
            let meta = aws_sdk_s3::error::ProvideErrorMetadata::meta(sdk_err);
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

/// Get a user-friendly suggestion for a known error code
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
