//! AWS resource scanner for finding nix-bench resources by tags
//!
//! Discovers resources directly from AWS APIs, independent of local database.
//! This enables cleanup of orphaned resources that were never recorded in the DB.

use crate::aws::context::AwsContext;
use anyhow::Result;
use aws_sdk_ec2::types::Filter;
use chrono::{DateTime, Duration, Utc};
use nix_bench_common::tags::{self, TAG_CREATED_AT, TAG_RUN_ID, TAG_STATUS, TAG_TOOL, TAG_TOOL_VALUE};
pub use nix_bench_common::ResourceKind;
use std::collections::HashMap;
use tracing::debug;

/// Discovered AWS resource from scanning
#[derive(Debug, Clone)]
pub struct DiscoveredResource {
    /// Type of resource
    pub resource_type: ResourceKind,
    /// AWS resource identifier
    pub resource_id: String,
    /// AWS region
    pub region: String,
    /// Run ID from tag
    pub run_id: String,
    /// Creation timestamp from tag
    pub created_at: DateTime<Utc>,
    /// Status from tag
    pub status: String,
    /// All tags on the resource
    pub tags: HashMap<String, String>,
}

/// Scanner configuration
#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// Only find resources older than this duration
    pub min_age: Duration,
    /// Only find resources from specific run
    pub run_id: Option<String>,
    /// Only find resources with specific status
    pub status: Option<String>,
    /// Include resources in "creating" status (be careful!)
    pub include_creating: bool,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            min_age: Duration::minutes(10), // Grace period
            run_id: None,
            status: None,
            include_creating: false,
        }
    }
}

/// Scanner for finding nix-bench resources in AWS
pub struct ResourceScanner {
    ctx: AwsContext,
    region: String,
}

impl ResourceScanner {
    /// Create a new scanner for the given region
    pub async fn new(region: &str) -> Result<Self> {
        let ctx = AwsContext::new(region).await;
        Ok(Self {
            ctx,
            region: region.to_string(),
        })
    }

    /// Scan all resource types and return discovered nix-bench resources
    pub async fn scan_all(&self, config: &ScanConfig) -> Result<Vec<DiscoveredResource>> {
        let mut resources = Vec::new();

        // Scan in parallel
        let (ec2, sg, s3, iam) = tokio::join!(
            self.scan_ec2_instances(config),
            self.scan_security_groups(config),
            self.scan_s3_buckets(config),
            self.scan_iam_resources(config),
        );

        resources.extend(ec2?);
        resources.extend(sg?);
        resources.extend(s3?);
        resources.extend(iam?);

        Ok(resources)
    }

    /// Scan EC2 instances by tag filter
    pub async fn scan_ec2_instances(
        &self,
        config: &ScanConfig,
    ) -> Result<Vec<DiscoveredResource>> {
        let client = self.ctx.ec2_client();

        let mut filters = vec![
            Filter::builder()
                .name(format!("tag:{}", TAG_TOOL))
                .values(TAG_TOOL_VALUE)
                .build(),
            // Exclude terminated instances
            Filter::builder()
                .name("instance-state-name")
                .values("pending")
                .values("running")
                .values("stopping")
                .values("stopped")
                .build(),
        ];

        if let Some(ref run_id) = config.run_id {
            filters.push(
                Filter::builder()
                    .name(format!("tag:{}", TAG_RUN_ID))
                    .values(run_id)
                    .build(),
            );
        }

        let response = client
            .describe_instances()
            .set_filters(Some(filters))
            .send()
            .await?;

        let mut resources = Vec::new();
        let now = Utc::now();

        for reservation in response.reservations() {
            for instance in reservation.instances() {
                let tags = extract_ec2_tags(instance.tags());

                if !self.should_include(&tags, config, now) {
                    continue;
                }

                if let Some(instance_id) = instance.instance_id() {
                    if let Some(run_id) = tags.get(TAG_RUN_ID) {
                        resources.push(DiscoveredResource {
                            resource_type: ResourceKind::Ec2Instance,
                            resource_id: instance_id.to_string(),
                            region: self.region.clone(),
                            run_id: run_id.clone(),
                            created_at: parse_created_at(&tags),
                            status: tags.get(TAG_STATUS).cloned().unwrap_or_default(),
                            tags,
                        });
                    }
                }
            }
        }

        debug!(count = resources.len(), "Found EC2 instances");
        Ok(resources)
    }

    /// Scan Security Groups by tag filter
    pub async fn scan_security_groups(
        &self,
        config: &ScanConfig,
    ) -> Result<Vec<DiscoveredResource>> {
        let client = self.ctx.ec2_client();

        let mut filters = vec![Filter::builder()
            .name(format!("tag:{}", TAG_TOOL))
            .values(TAG_TOOL_VALUE)
            .build()];

        if let Some(ref run_id) = config.run_id {
            filters.push(
                Filter::builder()
                    .name(format!("tag:{}", TAG_RUN_ID))
                    .values(run_id)
                    .build(),
            );
        }

        let response = client
            .describe_security_groups()
            .set_filters(Some(filters))
            .send()
            .await?;

        let mut resources = Vec::new();
        let now = Utc::now();

        for sg in response.security_groups() {
            let tags = extract_ec2_tags(sg.tags());

            if !self.should_include(&tags, config, now) {
                continue;
            }

            if let Some(sg_id) = sg.group_id() {
                if let Some(run_id) = tags.get(TAG_RUN_ID) {
                    resources.push(DiscoveredResource {
                        resource_type: ResourceKind::SecurityGroup,
                        resource_id: sg_id.to_string(),
                        region: self.region.clone(),
                        run_id: run_id.clone(),
                        created_at: parse_created_at(&tags),
                        status: tags.get(TAG_STATUS).cloned().unwrap_or_default(),
                        tags,
                    });
                }
            }
        }

        debug!(count = resources.len(), "Found security groups");
        Ok(resources)
    }

    /// Scan S3 buckets by listing and checking tags
    pub async fn scan_s3_buckets(&self, config: &ScanConfig) -> Result<Vec<DiscoveredResource>> {
        let client = self.ctx.s3_client();

        let list_response = client.list_buckets().send().await?;
        let mut resources = Vec::new();
        let now = Utc::now();

        for bucket in list_response.buckets() {
            let bucket_name = match bucket.name() {
                Some(n) => n,
                None => continue,
            };

            // Quick filter: nix-bench buckets start with "nix-bench-"
            if !bucket_name.starts_with("nix-bench-") {
                continue;
            }

            // Get bucket tags
            let tags_result = client.get_bucket_tagging().bucket(bucket_name).send().await;

            let tags = match tags_result {
                Ok(resp) => extract_s3_tags(resp.tag_set()),
                Err(_) => continue, // No tags or access denied
            };

            // Check if this is a nix-bench bucket
            if tags.get(TAG_TOOL) != Some(&TAG_TOOL_VALUE.to_string()) {
                continue;
            }

            if !self.should_include(&tags, config, now) {
                continue;
            }

            if let Some(run_id) = tags.get(TAG_RUN_ID) {
                resources.push(DiscoveredResource {
                    resource_type: ResourceKind::S3Bucket,
                    resource_id: bucket_name.to_string(),
                    region: self.region.clone(),
                    run_id: run_id.clone(),
                    created_at: parse_created_at(&tags),
                    status: tags.get(TAG_STATUS).cloned().unwrap_or_default(),
                    tags,
                });
            }
        }

        debug!(count = resources.len(), "Found S3 buckets");
        Ok(resources)
    }

    /// Scan IAM roles and instance profiles by tag
    pub async fn scan_iam_resources(&self, config: &ScanConfig) -> Result<Vec<DiscoveredResource>> {
        let client = self.ctx.iam_client();

        // List roles
        let roles_response = client.list_roles().send().await?;

        let mut resources = Vec::new();
        let now = Utc::now();

        for role in roles_response.roles() {
            let role_name = role.role_name();

            // Quick filter: nix-bench roles start with "nix-bench-agent-"
            if !role_name.starts_with("nix-bench-agent-") {
                continue;
            }

            // Get role tags
            let tags_result = client.list_role_tags().role_name(role_name).send().await;

            let tags = match tags_result {
                Ok(resp) => extract_iam_tags(resp.tags()),
                Err(_) => continue,
            };

            // Check if this is a nix-bench role
            if tags.get(TAG_TOOL) != Some(&TAG_TOOL_VALUE.to_string()) {
                continue;
            }

            if !self.should_include(&tags, config, now) {
                continue;
            }

            if let Some(run_id) = tags.get(TAG_RUN_ID) {
                resources.push(DiscoveredResource {
                    resource_type: ResourceKind::IamRole,
                    resource_id: role_name.to_string(),
                    region: self.region.clone(),
                    run_id: run_id.clone(),
                    created_at: parse_created_at(&tags),
                    status: tags.get(TAG_STATUS).cloned().unwrap_or_default(),
                    tags,
                });
            }
        }

        debug!(count = resources.len(), "Found IAM roles");
        Ok(resources)
    }

    /// Check if a resource should be included based on config
    fn should_include(
        &self,
        tags: &HashMap<String, String>,
        config: &ScanConfig,
        now: DateTime<Utc>,
    ) -> bool {
        // Check age filter
        if let Some(created_str) = tags.get(TAG_CREATED_AT) {
            if let Some(created) = tags::parse_created_at(created_str) {
                let age = now - created;
                if age < config.min_age {
                    return false; // Skip resources in grace period
                }
            }
        }

        // Check status filter
        let status = tags.get(TAG_STATUS).cloned().unwrap_or_default();
        if !config.include_creating && status == tags::status::CREATING {
            return false; // Skip resources still being created
        }

        if let Some(ref filter_status) = config.status {
            if &status != filter_status {
                return false;
            }
        }

        // Check run_id filter (already handled by tag filter for EC2, but double-check)
        if let Some(ref filter_run_id) = config.run_id {
            if tags.get(TAG_RUN_ID) != Some(filter_run_id) {
                return false;
            }
        }

        true
    }
}

fn extract_ec2_tags(tags: &[aws_sdk_ec2::types::Tag]) -> HashMap<String, String> {
    tags.iter()
        .filter_map(|t| match (t.key(), t.value()) {
            (Some(k), Some(v)) => Some((k.to_string(), v.to_string())),
            _ => None,
        })
        .collect()
}

fn extract_s3_tags(tags: &[aws_sdk_s3::types::Tag]) -> HashMap<String, String> {
    tags.iter()
        .map(|t| (t.key().to_string(), t.value().to_string()))
        .collect()
}

fn extract_iam_tags(tags: &[aws_sdk_iam::types::Tag]) -> HashMap<String, String> {
    tags.iter()
        .map(|t| (t.key().to_string(), t.value().to_string()))
        .collect()
}

fn parse_created_at(tags: &HashMap<String, String>) -> DateTime<Utc> {
    tags.get(TAG_CREATED_AT)
        .and_then(|s| tags::parse_created_at(s))
        .unwrap_or_else(Utc::now)
}
