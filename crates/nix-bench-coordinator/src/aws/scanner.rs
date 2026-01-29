//! AWS resource scanner for finding nix-bench resources by tags
//!
//! Discovers resources directly from AWS APIs, independent of local database.
//! This enables cleanup of orphaned resources that were never recorded in the DB.

use super::resource_guard::types::ResourceId;
use super::tags::{self, TAG_CREATED_AT, TAG_RUN_ID, TAG_STATUS, TAG_TOOL, TAG_TOOL_VALUE};
use crate::aws::context::AwsContext;
use anyhow::Result;
use aws_sdk_ec2::types::Filter;
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use tracing::debug;

/// Discovered AWS resource from scanning
#[derive(Debug, Clone)]
pub struct DiscoveredResource {
    /// Resource identifier (carries both kind and ID)
    pub resource: ResourceId,
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

/// Candidate resource for `build_discovered_resource`.
struct ResourceCandidate<'a> {
    make_resource: fn(String) -> ResourceId,
    resource_id: &'a str,
    tags: HashMap<String, String>,
    is_untagged_orphan: bool,
    fallback_created_at: Option<DateTime<Utc>>,
    name_prefix: &'a str,
}

/// IAM resource item used by `scan_iam_paginated`.
struct IamItem {
    name: String,
    created_at: Option<DateTime<Utc>>,
    tags: HashMap<String, String>,
    is_untagged: bool,
}

/// Scanner for finding nix-bench resources in AWS
pub struct ResourceScanner {
    ctx: AwsContext,
    region: String,
}

impl ResourceScanner {
    pub fn from_context(ctx: &AwsContext) -> Self {
        Self {
            ctx: ctx.clone(),
            region: ctx.region().to_string(),
        }
    }

    pub async fn new(region: &str) -> Result<Self> {
        let ctx = AwsContext::new(region).await;
        Ok(Self::from_context(&ctx))
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
    pub async fn scan_ec2_instances(&self, config: &ScanConfig) -> Result<Vec<DiscoveredResource>> {
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
                            resource: ResourceId::Ec2Instance(instance_id.to_string()),
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

        let mut filters = vec![
            Filter::builder()
                .name(format!("tag:{}", TAG_TOOL))
                .values(TAG_TOOL_VALUE)
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
                        resource: ResourceId::SecurityGroup(sg_id.to_string()),
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
                    debug!(bucket = %bucket_name, "Found untagged bucket with nix-bench- prefix");
                    (HashMap::new(), true)
                }
            };

            let fallback_created_at = bucket
                .creation_date()
                .and_then(|dt| DateTime::from_timestamp(dt.secs(), dt.subsec_nanos()));

            if let Some(resource) = self.build_discovered_resource(
                ResourceCandidate {
                    make_resource: ResourceId::S3Bucket,
                    resource_id: bucket_name,
                    tags,
                    is_untagged_orphan,
                    fallback_created_at,
                    name_prefix: "nix-bench-",
                },
                config,
                now,
            ) {
                resources.push(resource);
            }
        }

        debug!(count = resources.len(), "Found S3 buckets");
        Ok(resources)
    }

    /// Scan IAM roles by tag
    pub async fn scan_iam_roles(&self, config: &ScanConfig) -> Result<Vec<DiscoveredResource>> {
        let client = self.ctx.iam_client();

        self.scan_iam_paginated(config, ResourceId::IamRole, "IAM roles", |marker| {
            let c = client.clone();
            async move {
                let mut req = c.list_roles();
                if let Some(m) = &marker {
                    req = req.marker(m);
                }
                let resp = req.send().await?;
                let mut items = Vec::new();
                for r in resp.roles() {
                    let name = r.role_name().to_string();
                    let created_at = DateTime::from_timestamp(
                        r.create_date().secs(),
                        r.create_date().subsec_nanos(),
                    );
                    let tags_result = c.list_role_tags().role_name(&name).send().await;
                    let (tags, is_untagged) = match tags_result {
                        Ok(t) => (extract_iam_tags(t.tags()), false),
                        Err(_) => {
                            debug!(name = %name, "Couldn't get tags");
                            (HashMap::new(), true)
                        }
                    };
                    items.push(IamItem {
                        name,
                        created_at,
                        tags,
                        is_untagged,
                    });
                }
                let next = if resp.is_truncated() {
                    resp.marker().map(|s| s.to_string())
                } else {
                    None
                };
                Ok((items, next))
            }
        })
        .await
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

        self.scan_iam_paginated(
            config,
            ResourceId::IamInstanceProfile,
            "IAM instance profiles",
            |marker| {
                let c = client.clone();
                async move {
                    let mut req = c.list_instance_profiles();
                    if let Some(m) = &marker {
                        req = req.marker(m);
                    }
                    let resp = req.send().await?;
                    let mut items = Vec::new();
                    for p in resp.instance_profiles() {
                        let name = p.instance_profile_name().to_string();
                        let created_at = DateTime::from_timestamp(
                            p.create_date().secs(),
                            p.create_date().subsec_nanos(),
                        );
                        let tags_result = c
                            .list_instance_profile_tags()
                            .instance_profile_name(&name)
                            .send()
                            .await;
                        let (tags, is_untagged) = match tags_result {
                            Ok(t) => (extract_iam_tags(t.tags()), false),
                            Err(_) => {
                                debug!(name = %name, "Couldn't get tags");
                                (HashMap::new(), true)
                            }
                        };
                        items.push(IamItem {
                            name,
                            created_at,
                            tags,
                            is_untagged,
                        });
                    }
                    let next = if resp.is_truncated() {
                        resp.marker().map(|s| s.to_string())
                    } else {
                        None
                    };
                    Ok((items, next))
                }
            },
        )
        .await
    }

    /// Generic paginated IAM scanner that handles the shared pagination,
    /// name-prefix filtering, tag extraction, and resource building logic.
    async fn scan_iam_paginated<F, Fut>(
        &self,
        config: &ScanConfig,
        make_resource: fn(String) -> ResourceId,
        label: &str,
        list_page: F,
    ) -> Result<Vec<DiscoveredResource>>
    where
        F: Fn(Option<String>) -> Fut,
        Fut: std::future::Future<Output = Result<(Vec<IamItem>, Option<String>)>>,
    {
        let mut resources = Vec::new();
        let now = Utc::now();
        let mut marker: Option<String> = None;

        loop {
            let (items, next_marker) = list_page(marker).await?;

            for item in items {
                if !item.name.starts_with("nix-bench-agent-") {
                    continue;
                }

                if let Some(resource) = self.build_discovered_resource(
                    ResourceCandidate {
                        make_resource,
                        resource_id: &item.name,
                        tags: item.tags,
                        is_untagged_orphan: item.is_untagged,
                        fallback_created_at: item.created_at,
                        name_prefix: "nix-bench-agent-",
                    },
                    config,
                    now,
                ) {
                    resources.push(resource);
                }
            }

            match next_marker {
                Some(m) => marker = Some(m),
                None => break,
            }
        }

        debug!(count = resources.len(), "Found {}", label);
        Ok(resources)
    }

    /// Build a `DiscoveredResource` from tags and metadata, applying config filters.
    fn build_discovered_resource(
        &self,
        params: ResourceCandidate<'_>,
        config: &ScanConfig,
        now: DateTime<Utc>,
    ) -> Option<DiscoveredResource> {
        let ResourceCandidate {
            make_resource,
            resource_id,
            tags,
            is_untagged_orphan,
            fallback_created_at,
            name_prefix,
        } = params;
        // For tagged resources, verify the tool tag
        if !is_untagged_orphan && tags.get(TAG_TOOL) != Some(&TAG_TOOL_VALUE.to_string()) {
            return None;
        }

        let created_at = if is_untagged_orphan {
            fallback_created_at.unwrap_or_else(Utc::now)
        } else {
            parse_created_at(&tags)
        };

        // Apply age/status/run_id filters
        if is_untagged_orphan {
            let age = now - created_at;
            if age < config.min_age {
                return None;
            }
        } else if !self.should_include(&tags, config, now) {
            return None;
        }

        // Extract run_id: from tags if available, otherwise from resource name
        let run_id = if let Some(rid) = tags.get(TAG_RUN_ID) {
            rid.clone()
        } else {
            resource_id
                .strip_prefix(name_prefix)
                .unwrap_or("unknown")
                .to_string()
        };

        let status = if is_untagged_orphan {
            "orphaned".to_string()
        } else {
            tags.get(TAG_STATUS).cloned().unwrap_or_default()
        };

        Some(DiscoveredResource {
            resource: make_resource(resource_id.to_string()),
            region: self.region.clone(),
            run_id,
            created_at,
            status,
            tags,
        })
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

/// Extract tags from any AWS tag type into a HashMap.
///
/// Different AWS SDKs use different tag types (ec2::Tag, s3::Tag, iam::Tag)
/// but they all have key/value string fields. This generic function handles
/// them all via closures.
fn extract_tags<T>(
    tags: &[T],
    key: impl Fn(&T) -> Option<&str>,
    value: impl Fn(&T) -> Option<&str>,
) -> HashMap<String, String> {
    tags.iter()
        .filter_map(|t| match (key(t), value(t)) {
            (Some(k), Some(v)) => Some((k.to_string(), v.to_string())),
            _ => None,
        })
        .collect()
}

fn extract_ec2_tags(tags: &[aws_sdk_ec2::types::Tag]) -> HashMap<String, String> {
    extract_tags(tags, |t| t.key(), |t| t.value())
}

fn extract_s3_tags(tags: &[aws_sdk_s3::types::Tag]) -> HashMap<String, String> {
    extract_tags(tags, |t| Some(t.key()), |t| Some(t.value()))
}

fn extract_iam_tags(tags: &[aws_sdk_iam::types::Tag]) -> HashMap<String, String> {
    extract_tags(tags, |t| Some(t.key()), |t| Some(t.value()))
}

fn parse_created_at(tags: &HashMap<String, String>) -> DateTime<Utc> {
    tags.get(TAG_CREATED_AT)
        .and_then(|s| tags::parse_created_at(s))
        .unwrap_or_else(Utc::now)
}
