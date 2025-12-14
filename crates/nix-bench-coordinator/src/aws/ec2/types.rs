//! EC2 types and configuration

/// Launched instance info
#[derive(Debug, Clone)]
pub struct LaunchedInstance {
    pub instance_id: String,
    pub instance_type: String,
    pub system: String,
    pub public_ip: Option<String>,
}

/// Configuration for launching an EC2 instance
#[derive(Debug, Clone)]
pub struct LaunchInstanceConfig {
    /// Unique run identifier for tagging
    pub run_id: String,
    /// EC2 instance type (e.g., "c7i.xlarge")
    pub instance_type: String,
    /// System architecture ("x86_64-linux" or "aarch64-linux")
    pub system: String,
    /// User data script (will be base64 encoded)
    pub user_data: String,
    /// Optional VPC subnet ID
    pub subnet_id: Option<String>,
    /// Optional security group ID
    pub security_group_id: Option<String>,
    /// Optional IAM instance profile name
    pub iam_instance_profile: Option<String>,
}

impl LaunchInstanceConfig {
    /// Create a new launch configuration with required fields
    pub fn new(
        run_id: impl Into<String>,
        instance_type: impl Into<String>,
        system: impl Into<String>,
        user_data: impl Into<String>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            instance_type: instance_type.into(),
            system: system.into(),
            user_data: user_data.into(),
            subnet_id: None,
            security_group_id: None,
            iam_instance_profile: None,
        }
    }

    /// Set the VPC subnet ID
    pub fn with_subnet(mut self, subnet_id: impl Into<String>) -> Self {
        self.subnet_id = Some(subnet_id.into());
        self
    }

    /// Set the security group ID
    pub fn with_security_group(mut self, security_group_id: impl Into<String>) -> Self {
        self.security_group_id = Some(security_group_id.into());
        self
    }

    /// Set the IAM instance profile name
    pub fn with_iam_profile(mut self, profile_name: impl Into<String>) -> Self {
        self.iam_instance_profile = Some(profile_name.into());
        self
    }
}
