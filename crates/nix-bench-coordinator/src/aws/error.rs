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
        matches!(self, AwsError::IamPropagationDelay | AwsError::Throttled)
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

/// Classify an error from an anyhow::Error by extracting the error code.
///
/// This is a fallback for when we don't have direct access to the SDK error.
/// It uses string matching on the Debug output as a last resort.
pub fn classify_anyhow_error(error: &anyhow::Error) -> AwsError {
    let error_debug = format!("{:?}", error);

    // Check for not-found codes
    for code in NOT_FOUND_CODES {
        if error_debug.contains(code) {
            return AwsError::NotFound {
                resource_type: "resource",
                resource_id: code.to_string(),
            };
        }
    }

    // Check for already-exists codes
    for code in ALREADY_EXISTS_CODES {
        if error_debug.contains(code) {
            return AwsError::AlreadyExists;
        }
    }

    // Check for throttling codes
    for code in THROTTLING_CODES {
        if error_debug.contains(code) {
            return AwsError::Throttled;
        }
    }

    // Check for IAM propagation delay
    if (error_debug.contains("InvalidParameterValue") && error_debug.contains("iamInstanceProfile"))
        || error_debug.contains("Invalid IAM Instance Profile")
    {
        return AwsError::IamPropagationDelay;
    }

    // Fallback
    AwsError::Sdk {
        code: None,
        message: error.to_string(),
    }
}
