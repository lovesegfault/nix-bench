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
        let (ec2, sg, s3, iam_roles, iam_profiles) = tokio::join!(
            self.scan_ec2_instances(config),
            self.scan_security_groups(config),
            self.scan_s3_buckets(config),
            self.scan_iam_roles(config),
            self.scan_iam_instance_profiles(config),
        );

        resources.extend(ec2?);
        resources.extend(sg?);
        resources.extend(s3?);
        resources.extend(iam_roles?);
        resources.extend(iam_profiles?);

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
    ///
    /// This method handles two cases:
    /// 1. Properly tagged buckets: identified by `nix-bench:tool` tag
    /// 2. Orphaned untagged buckets: have `nix-bench-` prefix but no tags (e.g., crash between create and tag)
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

            // Get bucket tags - handle both tagged and untagged cases
            let tags_result = client.get_bucket_tagging().bucket(bucket_name).send().await;

            let (tags, is_untagged_orphan) = match tags_result {
                Ok(resp) => (extract_s3_tags(resp.tag_set()), false),
                Err(_) => {
                    // Bucket has nix-bench- prefix but no tags = likely orphaned
                    // Include it so it can be cleaned up
                    debug!(bucket = %bucket_name, "Found untagged bucket with nix-bench- prefix");
                    (HashMap::new(), true)
                }
            };

            // For tagged buckets, verify the tool tag
            if !is_untagged_orphan && tags.get(TAG_TOOL) != Some(&TAG_TOOL_VALUE.to_string()) {
                continue;
            }

            // For age filtering on untagged buckets, use the bucket creation date
            let created_at = if is_untagged_orphan {
                bucket
                    .creation_date()
                    .and_then(|dt| DateTime::from_timestamp(dt.secs(), dt.subsec_nanos()))
                    .unwrap_or_else(Utc::now)
            } else {
                parse_created_at(&tags)
            };

            // Apply age filter for untagged buckets
            if is_untagged_orphan {
                let age = now - created_at;
                if age < config.min_age {
                    debug!(bucket = %bucket_name, age_mins = age.num_minutes(), "Skipping untagged bucket in grace period");
                    continue;
                }
            } else if !self.should_include(&tags, config, now) {
                continue;
            }

            // Extract run_id: from tags if available, otherwise from bucket name
            // Bucket name format: nix-bench-{run_id}
            let run_id = if let Some(rid) = tags.get(TAG_RUN_ID) {
                rid.clone()
            } else {
                // Extract run_id from bucket name: "nix-bench-{run_id}" -> "{run_id}"
                bucket_name.strip_prefix("nix-bench-").unwrap_or("unknown").to_string()
            };

            let status = if is_untagged_orphan {
                "orphaned".to_string()
            } else {
                tags.get(TAG_STATUS).cloned().unwrap_or_default()
            };

            resources.push(DiscoveredResource {
                resource_type: ResourceKind::S3Bucket,
                resource_id: bucket_name.to_string(),
                region: self.region.clone(),
                run_id,
                created_at,
                status,
                tags,
            });
        }

        debug!(count = resources.len(), "Found S3 buckets");
        Ok(resources)
    }

    /// Scan IAM roles by tag
    pub async fn scan_iam_roles(&self, config: &ScanConfig) -> Result<Vec<DiscoveredResource>> {
        let client = self.ctx.iam_client();

        // List roles - handle pagination for large accounts
        let mut resources = Vec::new();
        let now = Utc::now();
        let mut marker: Option<String> = None;

        loop {
            let mut request = client.list_roles();
            if let Some(m) = &marker {
                request = request.marker(m);
            }

            let roles_response = request.send().await?;

            for role in roles_response.roles() {
                let role_name = role.role_name();

                // Quick filter: nix-bench roles start with "nix-bench-agent-"
                if !role_name.starts_with("nix-bench-agent-") {
                    continue;
                }

                // Get role tags
                let tags_result = client.list_role_tags().role_name(role_name).send().await;

                let (tags, is_untagged) = match tags_result {
                    Ok(resp) => (extract_iam_tags(resp.tags()), false),
                    Err(_) => {
                        // Role has prefix but no tags or access denied
                        debug!(role = %role_name, "Found role with prefix but couldn't get tags");
                        (HashMap::new(), true)
                    }
                };

                // For tagged roles, verify the tool tag
                if !is_untagged && tags.get(TAG_TOOL) != Some(&TAG_TOOL_VALUE.to_string()) {
                    continue;
                }

                // For untagged roles with the prefix, include them as orphans
                let created_at = if is_untagged {
                    let dt = role.create_date();
                    DateTime::from_timestamp(dt.secs(), dt.subsec_nanos())
                        .unwrap_or_else(Utc::now)
                } else {
                    parse_created_at(&tags)
                };

                if is_untagged {
                    let age = now - created_at;
                    if age < config.min_age {
                        continue;
                    }
                } else if !self.should_include(&tags, config, now) {
                    continue;
                }

                // Extract run_id from tags or role name
                // Role name format: nix-bench-agent-{truncated_run_id}
                let run_id = if let Some(rid) = tags.get(TAG_RUN_ID) {
                    rid.clone()
                } else {
                    role_name.strip_prefix("nix-bench-agent-").unwrap_or("unknown").to_string()
                };

                let status = if is_untagged {
                    "orphaned".to_string()
                } else {
                    tags.get(TAG_STATUS).cloned().unwrap_or_default()
                };

                resources.push(DiscoveredResource {
                    resource_type: ResourceKind::IamRole,
                    resource_id: role_name.to_string(),
                    region: self.region.clone(),
                    run_id,
                    created_at,
                    status,
                    tags,
                });
            }

            // Handle pagination
            if roles_response.is_truncated() {
                marker = roles_response.marker().map(|s| s.to_string());
            } else {
                break;
            }
        }

        debug!(count = resources.len(), "Found IAM roles");
        Ok(resources)
    }

    /// Scan IAM instance profiles by tag
    ///
    /// This catches orphaned instance profiles that may exist without their paired role
    /// (e.g., if role deletion succeeded but profile deletion failed).
    pub async fn scan_iam_instance_profiles(
        &self,
        config: &ScanConfig,
    ) -> Result<Vec<DiscoveredResource>> {
        let client = self.ctx.iam_client();

        let mut resources = Vec::new();
        let now = Utc::now();
        let mut marker: Option<String> = None;

        loop {
            let mut request = client.list_instance_profiles();
            if let Some(m) = &marker {
                request = request.marker(m);
            }

            let profiles_response = request.send().await?;

            for profile in profiles_response.instance_profiles() {
                let profile_name = profile.instance_profile_name();

                // Quick filter: nix-bench profiles start with "nix-bench-agent-"
                if !profile_name.starts_with("nix-bench-agent-") {
                    continue;
                }

                // Get profile tags
                let tags_result = client
                    .list_instance_profile_tags()
                    .instance_profile_name(profile_name)
                    .send()
                    .await;

                let (tags, is_untagged) = match tags_result {
                    Ok(resp) => (extract_iam_tags(resp.tags()), false),
                    Err(_) => {
                        debug!(profile = %profile_name, "Found profile with prefix but couldn't get tags");
                        (HashMap::new(), true)
                    }
                };

                // For tagged profiles, verify the tool tag
                if !is_untagged && tags.get(TAG_TOOL) != Some(&TAG_TOOL_VALUE.to_string()) {
                    continue;
                }

                let created_at = if is_untagged {
                    let dt = profile.create_date();
                    DateTime::from_timestamp(dt.secs(), dt.subsec_nanos())
                        .unwrap_or_else(Utc::now)
                } else {
                    parse_created_at(&tags)
                };

                if is_untagged {
                    let age = now - created_at;
                    if age < config.min_age {
                        continue;
                    }
                } else if !self.should_include(&tags, config, now) {
                    continue;
                }

                // Extract run_id from tags or profile name
                let run_id = if let Some(rid) = tags.get(TAG_RUN_ID) {
                    rid.clone()
                } else {
                    profile_name
                        .strip_prefix("nix-bench-agent-")
                        .unwrap_or("unknown")
                        .to_string()
                };

                let status = if is_untagged {
                    "orphaned".to_string()
                } else {
                    tags.get(TAG_STATUS).cloned().unwrap_or_default()
                };

                resources.push(DiscoveredResource {
                    resource_type: ResourceKind::IamInstanceProfile,
                    resource_id: profile_name.to_string(),
                    region: self.region.clone(),
                    run_id,
                    created_at,
                    status,
                    tags,
                });
            }

            // Handle pagination
            if profiles_response.is_truncated() {
                marker = profiles_response.marker().map(|s| s.to_string());
            } else {
                break;
            }
        }

        debug!(count = resources.len(), "Found IAM instance profiles");
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
