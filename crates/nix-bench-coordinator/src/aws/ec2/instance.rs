//! EC2 instance lifecycle operations

use super::Ec2Client;
use super::types::{LaunchInstanceConfig, LaunchedInstance};
use crate::aws::error::{AwsError, classify_anyhow_error};
use crate::aws::tags::{self, TAG_CREATED_AT, TAG_RUN_ID, TAG_STATUS, TAG_TOOL, TAG_TOOL_VALUE};
use crate::wait::{WaitConfig, wait_for_resource};
use anyhow::{Context, Result};
use aws_sdk_ec2::types::{InstanceStateName, InstanceType, ResourceType, Tag, TagSpecification};
use backon::{ExponentialBuilder, Retryable};
use chrono::Utc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Check if an error is retryable (IAM propagation delay or throttling)
fn is_retryable_launch_error(e: &anyhow::Error) -> bool {
    matches!(
        classify_anyhow_error(e),
        AwsError::IamPropagationDelay | AwsError::Throttled
    )
}

/// Internal parameters for do_launch_instance
pub(super) struct LaunchParams<'a> {
    ami_id: &'a str,
    instance_type_enum: InstanceType,
    run_id: &'a str,
    instance_type: &'a str,
    system: nix_bench_common::Architecture,
    user_data_b64: &'a str,
    subnet_id: Option<&'a str>,
    security_group_id: Option<&'a str>,
    iam_instance_profile: Option<&'a str>,
}

impl Ec2Client {
    /// Launch an EC2 instance with the given configuration
    ///
    /// Retries on transient errors including:
    /// - IAM eventual consistency (profile not yet visible to EC2)
    /// - AWS rate limiting (throttling)
    pub async fn launch_instance(&self, config: LaunchInstanceConfig) -> Result<LaunchedInstance> {
        let ami_id = self.get_al2023_ami(config.system.as_str()).await?;

        let instance_type_enum: InstanceType = config
            .instance_type
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid instance type: {}", config.instance_type))?;

        info!(
            instance_type = %config.instance_type,
            system = %config.system,
            ami = %ami_id,
            "Launching instance"
        );

        let user_data_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            config.user_data.as_bytes(),
        );

        let iam_profile = config.iam_instance_profile.clone();
        let instance_type_for_log = config.instance_type.clone();

        (|| async {
            self.do_launch_instance(LaunchParams {
                ami_id: &ami_id,
                instance_type_enum: instance_type_enum.clone(),
                run_id: &config.run_id,
                instance_type: &config.instance_type,
                system: config.system,
                user_data_b64: &user_data_b64,
                subnet_id: config.subnet_id.as_deref(),
                security_group_id: config.security_group_id.as_deref(),
                iam_instance_profile: iam_profile.as_deref(),
            })
            .await
        })
        .retry(
            ExponentialBuilder::default()
                .with_min_delay(Duration::from_secs(2))
                .with_max_delay(Duration::from_secs(30))
                .with_max_times(8),
        )
        .when(is_retryable_launch_error)
        .notify(|e, dur| {
            let err_type = classify_anyhow_error(e);
            match err_type {
                AwsError::IamPropagationDelay => {
                    warn!(
                        delay = ?dur,
                        instance_type = %instance_type_for_log,
                        error = %e,
                        "IAM instance profile not yet visible to EC2, retrying..."
                    );
                }
                AwsError::Throttled => {
                    warn!(
                        delay = ?dur,
                        instance_type = %instance_type_for_log,
                        error = %e,
                        "AWS rate limited, backing off..."
                    );
                }
                _ => {
                    warn!(
                        delay = ?dur,
                        instance_type = %instance_type_for_log,
                        error = %e,
                        "Transient error, retrying..."
                    );
                }
            }
        })
        .await
    }

    /// Internal method to perform the actual RunInstances call
    pub(super) async fn do_launch_instance(
        &self,
        params: LaunchParams<'_>,
    ) -> Result<LaunchedInstance> {
        use aws_sdk_ec2::types::{BlockDeviceMapping, EbsBlockDevice, VolumeType};
        use nix_bench_common::defaults::DEFAULT_ROOT_VOLUME_SIZE_GIB;

        let created_at = tags::format_created_at(Utc::now());
        let mut request = self
            .client
            .run_instances()
            .image_id(params.ami_id)
            .instance_type(params.instance_type_enum)
            .min_count(1)
            .max_count(1)
            .user_data(params.user_data_b64)
            .block_device_mappings(
                BlockDeviceMapping::builder()
                    .device_name("/dev/xvda")
                    .ebs(
                        EbsBlockDevice::builder()
                            .volume_size(DEFAULT_ROOT_VOLUME_SIZE_GIB)
                            .volume_type(VolumeType::Gp3)
                            .delete_on_termination(true)
                            .build(),
                    )
                    .build(),
            )
            .tag_specifications(
                TagSpecification::builder()
                    .resource_type(ResourceType::Instance)
                    .tags(Tag::builder().key(TAG_TOOL).value(TAG_TOOL_VALUE).build())
                    .tags(Tag::builder().key(TAG_RUN_ID).value(params.run_id).build())
                    .tags(
                        Tag::builder()
                            .key(TAG_CREATED_AT)
                            .value(&created_at)
                            .build(),
                    )
                    .tags(
                        Tag::builder()
                            .key(TAG_STATUS)
                            .value(tags::status::CREATING)
                            .build(),
                    )
                    .tags(
                        Tag::builder()
                            .key("Name")
                            .value(format!(
                                "nix-bench-{}-{}",
                                params.run_id, params.instance_type
                            ))
                            .build(),
                    )
                    .tags(
                        Tag::builder()
                            .key(tags::TAG_INSTANCE_TYPE)
                            .value(params.instance_type)
                            .build(),
                    )
                    .build(),
            );

        if let Some(subnet) = params.subnet_id {
            request = request.subnet_id(subnet);
        }

        if let Some(sg) = params.security_group_id {
            request = request.security_group_ids(sg);
        }

        if let Some(profile) = params.iam_instance_profile {
            request = request.iam_instance_profile(
                aws_sdk_ec2::types::IamInstanceProfileSpecification::builder()
                    .name(profile)
                    .build(),
            );
        }

        let response = request.send().await.context("Failed to launch instance")?;

        let instance = response
            .instances()
            .first()
            .context("No instance returned")?;

        let instance_id = instance
            .instance_id()
            .context("No instance ID")?
            .to_string();

        info!(instance_id = %instance_id, "Instance launched");

        Ok(LaunchedInstance {
            instance_id,
            instance_type: params.instance_type.to_string(),
            system: params.system,
            public_ip: None,
        })
    }

    /// Default timeout for waiting for instance to be running (10 minutes)
    const DEFAULT_WAIT_TIMEOUT_SECS: u64 = 600;

    /// Wait for an instance to be running and get its public IP
    pub async fn wait_for_running(
        &self,
        instance_id: &str,
        timeout_secs: Option<u64>,
    ) -> Result<Option<String>> {
        let timeout = Duration::from_secs(timeout_secs.unwrap_or(Self::DEFAULT_WAIT_TIMEOUT_SECS));
        info!(
            instance_id = %instance_id,
            timeout_secs = timeout.as_secs(),
            "Waiting for instance to be running"
        );

        let result = tokio::time::timeout(timeout, self.wait_for_running_inner(instance_id)).await;

        match result {
            Ok(inner_result) => inner_result,
            Err(_) => {
                warn!(
                    instance_id = %instance_id,
                    timeout_secs = timeout.as_secs(),
                    "Timed out waiting for instance to be running"
                );
                Err(anyhow::anyhow!(
                    "Timeout waiting for instance {} to be running after {}s",
                    instance_id,
                    timeout.as_secs()
                ))
            }
        }
    }

    /// Inner wait loop without timeout, using exponential backoff (2-15s).
    ///
    /// Uses `wait_for_resource` for backoff. The result cell captures the
    /// public IP from the successful check. The outer `wait_for_running`
    /// wraps this in `tokio::time::timeout`.
    async fn wait_for_running_inner(&self, instance_id: &str) -> Result<Option<String>> {
        use std::sync::Mutex;

        let result_ip: Mutex<Option<String>> = Mutex::new(None);

        wait_for_resource(
            WaitConfig {
                initial_delay: Duration::from_secs(2),
                max_delay: Duration::from_secs(15),
                timeout: Duration::from_secs(Self::DEFAULT_WAIT_TIMEOUT_SECS),
                jitter: 0.25,
            },
            None,
            || {
                let result_ip = &result_ip;
                async move {
                    let response = self
                        .client
                        .describe_instances()
                        .instance_ids(instance_id)
                        .send()
                        .await
                        .context("Failed to describe instance")?;

                    let instance = response
                        .reservations()
                        .first()
                        .and_then(|r| r.instances().first())
                        .context("Instance not found")?;

                    let state = instance
                        .state()
                        .and_then(|s| s.name())
                        .unwrap_or(&InstanceStateName::Pending);

                    match state {
                        InstanceStateName::Running => {
                            let public_ip = instance.public_ip_address().map(|s| s.to_string());
                            info!(instance_id = %instance_id, public_ip = ?public_ip, "Instance is running");
                            *result_ip.lock().unwrap() = public_ip;
                            Ok(true)
                        }
                        InstanceStateName::Pending => Ok(false),
                        _ => {
                            let state_reason = instance
                                .state_reason()
                                .map(|r| {
                                    format!(
                                        "Reason code: {}\nReason: {}",
                                        r.code().unwrap_or("unknown"),
                                        r.message().unwrap_or("no message provided")
                                    )
                                })
                                .unwrap_or_else(|| {
                                    "No state reason provided by AWS".to_string()
                                });

                            anyhow::bail!(
                                "Instance {} entered unexpected state: {:?}\n{}",
                                instance_id,
                                state,
                                state_reason
                            );
                        }
                    }
                }
            },
            &format!("EC2 instance {} running", instance_id),
        )
        .await?;

        Ok(result_ip.into_inner().unwrap())
    }

    /// Terminate an instance
    pub async fn terminate_instance(&self, instance_id: &str) -> Result<()> {
        info!(instance_id = %instance_id, "Terminating instance");

        self.client
            .terminate_instances()
            .instance_ids(instance_id)
            .send()
            .await
            .context("Failed to terminate instance")?;

        Ok(())
    }

    /// Wait for an instance to be fully terminated, using exponential backoff (2-15s)
    pub async fn wait_for_terminated(&self, instance_id: &str) -> Result<()> {
        use nix_bench_common::defaults::DEFAULT_TERMINATION_WAIT_TIMEOUT_SECS;

        let result = wait_for_resource(
            WaitConfig {
                initial_delay: Duration::from_secs(2),
                max_delay: Duration::from_secs(15),
                timeout: Duration::from_secs(DEFAULT_TERMINATION_WAIT_TIMEOUT_SECS),
                jitter: 0.25,
            },
            None,
            || async {
                let response = self
                    .client
                    .describe_instances()
                    .instance_ids(instance_id)
                    .send()
                    .await;

                match response {
                    Ok(resp) => {
                        let state = resp
                            .reservations()
                            .first()
                            .and_then(|r| r.instances().first())
                            .and_then(|i| i.state())
                            .and_then(|s| s.name());

                        match state {
                            Some(InstanceStateName::Terminated) => {
                                debug!(instance_id = %instance_id, "Instance terminated");
                                Ok(true)
                            }
                            None => Ok(true), // No state info = gone
                            _ => Ok(false),   // Still shutting down or other state
                        }
                    }
                    Err(e) => {
                        let err = anyhow::Error::from(e);
                        if classify_anyhow_error(&err).is_not_found() {
                            Ok(true) // Already gone
                        } else {
                            warn!(instance_id = %instance_id, error = ?err, "Error checking instance state");
                            Ok(false) // Transient error, retry
                        }
                    }
                }
            },
            &format!("EC2 instance {} terminated", instance_id),
        )
        .await;

        // Timeout on termination wait is not an error — best-effort
        if let Err(e) = &result {
            if e.to_string().contains("Timeout") {
                warn!(instance_id = %instance_id, "Timeout waiting for instance to terminate");
                return Ok(());
            }
        }

        result
    }

    /// Terminate multiple instances in a single API call
    pub async fn terminate_instances(&self, instance_ids: &[String]) -> Result<()> {
        if instance_ids.is_empty() {
            return Ok(());
        }

        info!(count = instance_ids.len(), "Terminating instances in batch");

        self.client
            .terminate_instances()
            .set_instance_ids(Some(instance_ids.to_vec()))
            .send()
            .await
            .context("Failed to terminate instances")?;

        Ok(())
    }

    /// Wait for multiple instances to be fully terminated in parallel
    pub async fn wait_for_all_terminated(&self, instance_ids: &[String]) -> Result<()> {
        use futures::future::join_all;

        if instance_ids.is_empty() {
            return Ok(());
        }

        let futures: Vec<_> = instance_ids
            .iter()
            .map(|id| self.wait_for_terminated(id))
            .collect();

        let results = join_all(futures).await;

        // Log any errors but don't fail - we tried our best
        for (id, result) in instance_ids.iter().zip(results) {
            if let Err(e) = result {
                warn!(instance_id = %id, error = ?e, "Error waiting for instance termination");
            }
        }

        Ok(())
    }

    /// Get console output from an instance
    pub async fn get_console_output(&self, instance_id: &str) -> Result<Option<String>> {
        let response = self
            .client
            .get_console_output()
            .instance_id(instance_id)
            .send()
            .await
            .context("Failed to get console output")?;

        if let Some(encoded) = response.output() {
            use base64::Engine;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok());
            Ok(decoded)
        } else {
            Ok(None)
        }
    }

    /// Release an Elastic IP address
    ///
    /// Returns Ok(()) if the EIP was released or if it doesn't exist (idempotent for cleanup).
    pub async fn release_elastic_ip(&self, allocation_id: &str) -> Result<()> {
        info!(allocation_id = %allocation_id, "Releasing Elastic IP");

        match self
            .client
            .release_address()
            .allocation_id(allocation_id)
            .send()
            .await
        {
            Ok(_) => {
                info!(allocation_id = %allocation_id, "Released Elastic IP");
                Ok(())
            }
            Err(e) => {
                // Handle "not found" gracefully
                let error_str = format!("{:?}", e);
                if error_str.contains("InvalidAllocationID.NotFound") {
                    info!(allocation_id = %allocation_id, "Elastic IP already released");
                    Ok(())
                } else {
                    Err(anyhow::anyhow!("Failed to release Elastic IP: {}", e))
                }
            }
        }
    }
}
