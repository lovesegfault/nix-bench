//! Lifecycle state and initialization phases

/// Initialization phase
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitPhase {
    Starting,
    CreatingBucket,
    CreatingIamRole,
    UploadingAgents,
    LaunchingInstances,
    WaitingForInstances,
    Running,
    CleaningUp(CleanupProgress),
    Completed,
    Failed(String),
}

impl InitPhase {
    pub fn message(&self) -> &str {
        match self {
            InitPhase::Starting => "Starting...",
            InitPhase::CreatingBucket => "Creating S3 bucket...",
            InitPhase::CreatingIamRole => "Creating IAM role...",
            InitPhase::UploadingAgents => "Uploading agent binaries...",
            InitPhase::LaunchingInstances => "Launching EC2 instances...",
            InitPhase::WaitingForInstances => "Waiting for instances to start...",
            InitPhase::Running => "Running benchmarks...",
            InitPhase::CleaningUp(progress) => &progress.current_step,
            InitPhase::Completed => "Completed!",
            InitPhase::Failed(msg) => msg,
        }
    }
}

/// Progress tracking for cleanup phase
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CleanupProgress {
    /// EC2 instances: (completed, total)
    pub ec2_instances: (usize, usize),
    /// S3 bucket deleted
    pub s3_bucket: bool,
    /// IAM roles: (completed, total)
    pub iam_roles: (usize, usize),
    /// Security group rules: (completed, total)
    pub security_rules: (usize, usize),
    /// Current step description
    pub current_step: String,
}

impl CleanupProgress {
    /// Create a new cleanup progress tracker with initial totals
    pub fn new(
        ec2_total: usize,
        _eip_total: usize, // Kept for API compatibility, but unused
        iam_total: usize,
        sg_total: usize,
    ) -> Self {
        Self {
            ec2_instances: (0, ec2_total),
            s3_bucket: false,
            iam_roles: (0, iam_total),
            security_rules: (0, sg_total),
            current_step: "Starting cleanup...".to_string(),
        }
    }

    /// Calculate overall completion percentage
    pub fn percentage(&self) -> f64 {
        let total_items = self.ec2_instances.1
            + 1 // S3 bucket
            + self.iam_roles.1
            + self.security_rules.1;
        if total_items == 0 {
            return 100.0;
        }
        let completed = self.ec2_instances.0
            + (if self.s3_bucket { 1 } else { 0 })
            + self.iam_roles.0
            + self.security_rules.0;
        (completed as f64 / total_items as f64) * 100.0
    }
}

/// Lifecycle state for the application
#[derive(Debug)]
pub struct LifecycleState {
    /// Whether the app should quit
    pub should_quit: bool,
    /// Current initialization phase
    pub init_phase: InitPhase,
}

impl Default for LifecycleState {
    fn default() -> Self {
        Self {
            should_quit: false,
            init_phase: InitPhase::Starting,
        }
    }
}
