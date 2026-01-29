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
    pub fn is_not_found(&self) -> bool {
        matches!(self, AwsError::NotFound { .. })
    }

    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            AwsError::IamPropagationDelay | AwsError::Throttled | AwsError::DependencyViolation
        )
    }

    pub fn is_already_exists(&self) -> bool {
        matches!(self, AwsError::AlreadyExists)
    }

    pub fn suggestion(&self) -> Option<&'static str> {
        let code = match self {
            AwsError::Sdk { code: Some(c), .. } => c.as_str(),
            AwsError::Throttled => "Throttling",
            _ => return None,
        };
        ERROR_CODES
            .iter()
            .find(|e| e.code == code)
            .and_then(|e| e.suggestion)
    }
}

// --- Unified error code table ---

#[derive(Debug, Clone, Copy)]
enum ErrorCategory {
    NotFound,
    AlreadyExists,
    Throttling,
    Dependency,
    /// Codes that are only used for suggestions (no classification effect)
    SuggestionOnly,
}

struct ErrorCodeEntry {
    code: &'static str,
    category: ErrorCategory,
    suggestion: Option<&'static str>,
}

const ERROR_CODES: &[ErrorCodeEntry] = &[
    // Not found
    ErrorCodeEntry {
        code: "InvalidInstanceID.NotFound",
        category: ErrorCategory::NotFound,
        suggestion: None,
    },
    ErrorCodeEntry {
        code: "InvalidAllocationID.NotFound",
        category: ErrorCategory::NotFound,
        suggestion: None,
    },
    ErrorCodeEntry {
        code: "InvalidGroup.NotFound",
        category: ErrorCategory::NotFound,
        suggestion: None,
    },
    ErrorCodeEntry {
        code: "InvalidPermission.NotFound",
        category: ErrorCategory::NotFound,
        suggestion: None,
    },
    ErrorCodeEntry {
        code: "NoSuchBucket",
        category: ErrorCategory::NotFound,
        suggestion: None,
    },
    ErrorCodeEntry {
        code: "NoSuchKey",
        category: ErrorCategory::NotFound,
        suggestion: None,
    },
    ErrorCodeEntry {
        code: "NoSuchEntity",
        category: ErrorCategory::NotFound,
        suggestion: None,
    },
    // Already exists
    ErrorCodeEntry {
        code: "InvalidPermission.Duplicate",
        category: ErrorCategory::AlreadyExists,
        suggestion: None,
    },
    ErrorCodeEntry {
        code: "InvalidGroup.Duplicate",
        category: ErrorCategory::AlreadyExists,
        suggestion: None,
    },
    ErrorCodeEntry {
        code: "EntityAlreadyExists",
        category: ErrorCategory::AlreadyExists,
        suggestion: None,
    },
    ErrorCodeEntry {
        code: "BucketAlreadyOwnedByYou",
        category: ErrorCategory::AlreadyExists,
        suggestion: None,
    },
    // Throttling
    ErrorCodeEntry {
        code: "Throttling",
        category: ErrorCategory::Throttling,
        suggestion: Some("AWS API rate limit hit. The operation will be retried automatically."),
    },
    ErrorCodeEntry {
        code: "ThrottlingException",
        category: ErrorCategory::Throttling,
        suggestion: Some("AWS API rate limit hit. The operation will be retried automatically."),
    },
    ErrorCodeEntry {
        code: "RequestLimitExceeded",
        category: ErrorCategory::Throttling,
        suggestion: Some("AWS API rate limit hit. The operation will be retried automatically."),
    },
    // Dependency
    ErrorCodeEntry {
        code: "DependencyViolation",
        category: ErrorCategory::Dependency,
        suggestion: None,
    },
    // Capacity (suggestion-only)
    ErrorCodeEntry {
        code: "InsufficientInstanceCapacity",
        category: ErrorCategory::SuggestionOnly,
        suggestion: Some("Try a different availability zone or instance type."),
    },
    ErrorCodeEntry {
        code: "InsufficientHostCapacity",
        category: ErrorCategory::SuggestionOnly,
        suggestion: Some("Try a different availability zone or instance type."),
    },
    ErrorCodeEntry {
        code: "InsufficientCapacity",
        category: ErrorCategory::SuggestionOnly,
        suggestion: Some("Try a different availability zone or instance type."),
    },
    ErrorCodeEntry {
        code: "InsufficientReservedInstanceCapacity",
        category: ErrorCategory::SuggestionOnly,
        suggestion: Some("Your reserved instance capacity is exhausted. Try on-demand instances."),
    },
    // Limits (suggestion-only)
    ErrorCodeEntry {
        code: "InstanceLimitExceeded",
        category: ErrorCategory::SuggestionOnly,
        suggestion: Some("Request a service limit increase via AWS Service Quotas console."),
    },
    ErrorCodeEntry {
        code: "VcpuLimitExceeded",
        category: ErrorCategory::SuggestionOnly,
        suggestion: Some("Request a service limit increase via AWS Service Quotas console."),
    },
    ErrorCodeEntry {
        code: "MaxSpotInstanceCountExceeded",
        category: ErrorCategory::SuggestionOnly,
        suggestion: Some("Reduce spot instance count or request a limit increase."),
    },
    // Unsupported (suggestion-only)
    ErrorCodeEntry {
        code: "Unsupported",
        category: ErrorCategory::SuggestionOnly,
        suggestion: Some("This instance type may not be available in this region/AZ."),
    },
    ErrorCodeEntry {
        code: "UnsupportedOperation",
        category: ErrorCategory::SuggestionOnly,
        suggestion: Some("This instance type may not be available in this region/AZ."),
    },
];

/// Classify an AWS SDK error using the error code.
pub fn classify_aws_error(code: Option<&str>, message: Option<&str>) -> AwsError {
    let message = message.unwrap_or("Unknown error").to_string();

    if let Some(c) = code {
        if let Some(entry) = ERROR_CODES.iter().find(|e| e.code == c) {
            return match entry.category {
                ErrorCategory::NotFound => AwsError::NotFound {
                    resource_type: "resource",
                    resource_id: message,
                },
                ErrorCategory::AlreadyExists => AwsError::AlreadyExists,
                ErrorCategory::Throttling => AwsError::Throttled,
                ErrorCategory::Dependency => AwsError::DependencyViolation,
                ErrorCategory::SuggestionOnly => AwsError::Sdk {
                    code: Some(c.to_string()),
                    message,
                },
            };
        }
        // Special case: IAM propagation delay
        if c == "InvalidParameterValue" && message.contains("iamInstanceProfile") {
            return AwsError::IamPropagationDelay;
        }
        if message.contains("Invalid IAM Instance Profile") {
            return AwsError::IamPropagationDelay;
        }
    }

    AwsError::Sdk {
        code: code.map(|s| s.to_string()),
        message,
    }
}

/// Classify an error from an anyhow::Error by extracting the AWS error code.
///
/// Walks the error chain trying to downcast to known AWS SDK error types
/// to extract `.code()` and `.message()` via `ProvideErrorMetadata`.
pub fn classify_anyhow_error(error: &anyhow::Error) -> AwsError {
    macro_rules! try_classify {
        ($cause:expr, $($error_type:ty),+ $(,)?) => {
            $(
                if let Some(e) = $cause.downcast_ref::<aws_sdk_ec2::error::SdkError<$error_type>>() {
                    let meta = aws_sdk_ec2::error::ProvideErrorMetadata::meta(e);
                    return classify_aws_error(meta.code(), meta.message());
                }
            )+
        };
    }

    for cause in error.chain() {
        try_classify!(
            cause,
            aws_sdk_ec2::operation::run_instances::RunInstancesError,
            aws_sdk_ec2::operation::describe_instances::DescribeInstancesError,
            aws_sdk_ec2::operation::terminate_instances::TerminateInstancesError,
            aws_sdk_ec2::operation::create_security_group::CreateSecurityGroupError,
            aws_sdk_ec2::operation::delete_security_group::DeleteSecurityGroupError,
            aws_sdk_s3::operation::delete_bucket::DeleteBucketError,
            aws_sdk_s3::operation::create_bucket::CreateBucketError,
            aws_sdk_iam::operation::create_role::CreateRoleError,
            aws_sdk_iam::operation::delete_role::DeleteRoleError,
        );
    }

    AwsError::Sdk {
        code: None,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_not_found() {
        for code in ["InvalidInstanceID.NotFound", "NoSuchBucket", "NoSuchEntity"] {
            let err = classify_aws_error(Some(code), Some("msg"));
            assert!(err.is_not_found(), "Expected NotFound for code: {code}");
        }
    }

    #[test]
    fn classify_already_exists() {
        for code in ["EntityAlreadyExists", "BucketAlreadyOwnedByYou"] {
            let err = classify_aws_error(Some(code), Some("msg"));
            assert!(
                err.is_already_exists(),
                "Expected AlreadyExists for code: {code}"
            );
        }
    }

    #[test]
    fn classify_retryable() {
        let err = classify_aws_error(Some("Throttling"), Some("msg"));
        assert!(matches!(err, AwsError::Throttled));
        assert!(err.is_retryable());

        let err = classify_aws_error(Some("DependencyViolation"), Some("ENI attached"));
        assert!(matches!(err, AwsError::DependencyViolation));
        assert!(err.is_retryable());
    }

    #[test]
    fn iam_propagation_delay() {
        let err = classify_aws_error(
            Some("InvalidParameterValue"),
            Some("Value for parameter iamInstanceProfile is invalid"),
        );
        assert!(matches!(err, AwsError::IamPropagationDelay));

        let err = classify_aws_error(Some("SomeCode"), Some("Invalid IAM Instance Profile name"));
        assert!(matches!(err, AwsError::IamPropagationDelay));
    }

    #[test]
    fn unknown_codes_fall_through() {
        let err = classify_aws_error(Some("SomeNewError"), Some("details"));
        assert!(matches!(err, AwsError::Sdk { .. }));

        let err = classify_aws_error(None, Some("something failed"));
        assert!(matches!(err, AwsError::Sdk { code: None, .. }));
    }

    #[test]
    fn suggestions() {
        let err = classify_aws_error(Some("InsufficientInstanceCapacity"), Some("msg"));
        assert!(err.suggestion().is_some());

        let err = classify_aws_error(Some("Throttling"), Some("msg"));
        assert!(err.suggestion().is_some());

        let err = classify_aws_error(None, Some("msg"));
        assert!(err.suggestion().is_none());
    }
}
